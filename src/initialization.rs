//! Source-ordered initialization selected by the newly created database files.
//!
//! Every unit keeps its source form for the both-files initialization, and a
//! rebound form for each fresh-file selection that needs the objects it names
//! at their final names. A selection whose rebinding needs source history the
//! target file does not have is unavailable on its own, and the other
//! selections stay usable.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::iter::FusedIterator;

use crate::{
    errors::{Error, TranslationRefusal},
    history::placement::Files,
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

/// Each unit's form for one fresh-file selection, `None` where the unit runs
/// its emitted form or does not run, or why the selection is unavailable.
pub(crate) type Forms<S> = Result<Vec<Option<Vec<S>>>, InitializationError>;

/// One source unit and its forms for the selections that run it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Step<S> {
    /// The fresh files whose selection runs the unit.
    files: Files,
    /// The emitted form, run by the both-files initialization.
    base: Vec<S>,
    /// The fresh-main form, when rebinding changed it or the unit belongs to
    /// the existing attached file and runs only its connection objects.
    main_rebound: Option<Vec<S>>,
    /// The fresh-attached form, when rebinding changed it or the unit belongs
    /// to the existing main file and runs only its connection objects.
    attached_rebound: Option<Vec<S>>,
}

impl<S> Step<S> {
    /// The statements `selection` runs for this unit.
    fn form(&self, selection: FreshFiles) -> &[S] {
        let (rebound, file) = match selection {
            FreshFiles::Both => return &self.base,
            FreshFiles::Main => (&self.main_rebound, Files::MAIN),
            FreshFiles::Attached => (&self.attached_rebound, Files::ATTACHED),
        };
        match rebound {
            Some(rebound) => rebound,
            None if self.files.contains(file) => &self.base,
            None => &[],
        }
    }
}

/// A source-ordered initializer with prevalidated fresh-file selections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializationPlan<S> {
    steps: Vec<Step<S>>,
    /// Why the fresh-main selection cannot be produced.
    main_unavailable: Option<InitializationError>,
    /// Why the fresh-attached selection cannot be produced.
    attached_unavailable: Option<InitializationError>,
}

impl<S> InitializationPlan<S> {
    /// Assembles the plan from the main-file `prelude`, every unit's files and
    /// emitted form, and the forms of both fresh-file selections.
    pub(crate) fn new(
        prelude: Option<S>,
        units: impl IntoIterator<Item = (Files, Vec<S>)>,
        main: Forms<S>,
        attached: Forms<S>,
    ) -> Self {
        let (main_unavailable, main) = split(main);
        let (attached_unavailable, attached) = split(attached);
        let (mut main, mut attached) = (main.into_iter(), attached.into_iter());
        let prelude = prelude.map(|statement| {
            Step {
                files: Files::MAIN,
                base: alloc::vec![statement],
                main_rebound: None,
                attached_rebound: None,
            }
        });
        let steps = prelude
            .into_iter()
            .chain(units.into_iter().map(|(files, base)| {
                Step {
                    files,
                    base,
                    main_rebound: main.next().flatten(),
                    attached_rebound: attached.next().flatten(),
                }
            }))
            .collect();
        Self { steps, main_unavailable, attached_unavailable }
    }

    /// The number of statements needed when every configured file is new.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.iter().map(|step| step.base.len()).sum()
    }

    /// Whether complete initialization contains no statements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.iter().all(|step| step.base.is_empty())
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
            FreshFiles::Main => self.main_unavailable.as_ref(),
            FreshFiles::Attached => self.attached_unavailable.as_ref(),
            FreshFiles::Both => None,
        };
        if let Some(reason) = unavailable {
            return Err(reason.clone().into());
        }
        Ok(self.selected(files))
    }

    fn selected(&self, files: FreshFiles) -> SelectionIter<'_, S> {
        let remaining = self.steps.iter().map(|step| step.form(files).len()).sum();
        SelectionIter { steps: self.steps.iter(), files, within: [].iter(), remaining }
    }

    pub(crate) fn into_statements(self) -> impl Iterator<Item = S> {
        self.steps.into_iter().flat_map(|step| step.base)
    }

    /// Every statement of every form, rebound ones included.
    pub(crate) fn statements_mut(&mut self) -> impl Iterator<Item = &mut S> {
        self.steps.iter_mut().flat_map(|step| {
            step.base
                .iter_mut()
                .chain(step.main_rebound.iter_mut().flatten())
                .chain(step.attached_rebound.iter_mut().flatten())
        })
    }

    pub(crate) fn map<T>(self, mut f: impl FnMut(S) -> T) -> InitializationPlan<T> {
        let mut forms = |statements: Vec<S>| statements.into_iter().map(&mut f).collect();
        let steps = self
            .steps
            .into_iter()
            .map(|step| {
                Step {
                    files: step.files,
                    base: forms(step.base),
                    main_rebound: step.main_rebound.map(&mut forms),
                    attached_rebound: step.attached_rebound.map(&mut forms),
                }
            })
            .collect();
        InitializationPlan {
            steps,
            main_unavailable: self.main_unavailable,
            attached_unavailable: self.attached_unavailable,
        }
    }
}

fn split<S>(forms: Forms<S>) -> (Option<InitializationError>, Vec<Option<Vec<S>>>) {
    match forms {
        Ok(forms) => (None, forms),
        Err(error) => (Some(error), Vec::new()),
    }
}

struct SelectionIter<'a, S> {
    steps: core::slice::Iter<'a, Step<S>>,
    files: FreshFiles,
    within: core::slice::Iter<'a, S>,
    remaining: usize,
}

impl<'a, S> Iterator for SelectionIter<'a, S> {
    type Item = &'a S;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(statement) = self.within.next() {
                self.remaining -= 1;
                return Some(statement);
            }
            self.within = self.steps.next()?.form(self.files).iter();
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<S> ExactSizeIterator for SelectionIter<'_, S> {}
impl<S> FusedIterator for SelectionIter<'_, S> {}
