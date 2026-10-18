//! Source-ordered initialization selected by newly created database files.

pub(crate) mod compiler;

use alloc::vec::Vec;
use core::{iter::FusedIterator, slice};

use crate::{
    errors::{Error, TranslationRefusal},
    script::{ConnectionError, FreshFiles},
};

/// A fresh-file selection that cannot preserve initialization semantics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InitializationError {
    /// An attached-file initializer was requested without attached storage.
    #[error("the script has no attached database to initialize")]
    NoAttachedDatabase,
    /// Existing file state cannot satisfy a source-ordered initializer.
    #[error(transparent)]
    Unrepresentable(#[from] TranslationRefusal),
    /// A fresh-file initializer needs source history unavailable to the target
    /// file.
    #[error(transparent)]
    Unavailable(#[from] ConnectionError),
}

/// A source-ordered initializer with prevalidated fresh-file selections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializationPlan<S> {
    steps: Vec<InitializationStep<S>>,
    unavailable: [Option<InitializationError>; 2],
    counts: [usize; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitializationStep<S> {
    pub(crate) modes: u8,
    pub(crate) statement: S,
}

impl<S> InitializationPlan<S> {
    pub(crate) fn new(
        steps: Vec<InitializationStep<S>>,
        unavailable: [Option<InitializationError>; 2],
    ) -> Self {
        let mut counts = [0; 3];
        for step in &steps {
            for (count, mode) in
                counts.iter_mut().zip([FreshFiles::Main, FreshFiles::Attached, FreshFiles::Both])
            {
                *count += usize::from(step.modes & mode.mask() != 0);
            }
        }
        Self { steps, unavailable, counts }
    }

    /// The number of statements needed when every configured file is new.
    #[must_use]
    pub fn len(&self) -> usize {
        self.counts[2]
    }

    /// Whether complete initialization contains no statements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Complete initialization in source order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &S> + FusedIterator {
        self.selected(FreshFiles::Both)
    }

    pub(crate) fn select(
        &self,
        files: FreshFiles,
    ) -> Result<impl ExactSizeIterator<Item = &S> + FusedIterator, Error> {
        let unavailable = match files {
            FreshFiles::Main => self.unavailable[0].as_ref(),
            FreshFiles::Attached => self.unavailable[1].as_ref(),
            FreshFiles::Both => None,
        };
        if let Some(reason) = unavailable {
            return Err(reason.clone().into());
        }
        Ok(self.selected(files))
    }

    fn selected(&self, files: FreshFiles) -> InitializationIter<'_, S> {
        let index = match files {
            FreshFiles::Main => 0,
            FreshFiles::Attached => 1,
            FreshFiles::Both => 2,
        };
        InitializationIter {
            steps: self.steps.iter(),
            mode: files.mask(),
            remaining: self.counts[index],
        }
    }

    pub(crate) fn into_statements(self) -> impl Iterator<Item = S> {
        self.steps.into_iter().filter_map(|step| {
            (step.modes & FreshFiles::Both.mask() != 0).then_some(step.statement)
        })
    }

    pub(crate) fn map<T>(self, mut f: impl FnMut(S) -> T) -> InitializationPlan<T> {
        InitializationPlan {
            steps: self
                .steps
                .into_iter()
                .map(|step| InitializationStep { modes: step.modes, statement: f(step.statement) })
                .collect(),
            unavailable: self.unavailable,
            counts: self.counts,
        }
    }
}

struct InitializationIter<'a, S> {
    steps: slice::Iter<'a, InitializationStep<S>>,
    mode: u8,
    remaining: usize,
}

impl<'a, S> Iterator for InitializationIter<'a, S> {
    type Item = &'a S;

    fn next(&mut self) -> Option<Self::Item> {
        for step in self.steps.by_ref() {
            if step.modes & self.mode != 0 {
                self.remaining -= 1;
                return Some(&step.statement);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<S> ExactSizeIterator for InitializationIter<'_, S> {}
impl<S> FusedIterator for InitializationIter<'_, S> {}
