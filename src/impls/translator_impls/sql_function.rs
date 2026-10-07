//! Inlines scalar `LANGUAGE sql` function bodies.

use alloc::borrow::Cow;
#[cfg(not(feature = "std"))]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    ops::ControlFlow,
};

use sql_traits::{
    structs::{IdentifierCase, ParserDB, TargetName},
    traits::{DatabaseLike, FunctionLike, RoleLike, TableLike},
};
use sqlparser::{
    ast::{
        ArgMode, BinaryOperator, CastKind, CreateFunction, Cte, DataType, Distinct, Expr, Function,
        FunctionArg, FunctionArgExpr, FunctionArguments, FunctionCalledOnNull,
        FunctionDefinitionSetParam, FunctionReturnType, FunctionSecurity, FunctionSetValue,
        GroupByExpr, Ident, Join, JoinConstraint, JoinOperator, LimitClause, NamedWindowDefinition,
        NamedWindowExpr, ObjectName, ObjectNamePart, Offset, OperateFunctionArg, OrderBy,
        OrderByExpr, OrderByKind, Parens, Query, Select, SelectFlavor, SelectItem,
        SelectItemQualifiedWildcardKind, SetExpr, Statement, TableAlias, TableFactor,
        TableWithJoins, UnaryOperator, Value, ValueWithSpan, Visit, Visitor, WindowFrame,
        WindowFrameBound, WindowSpec, With,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
    tokenizer::Span,
};

use crate::{
    errors::Error,
    impls::{
        expr_helpers::{case_when, try_map_expr_children},
        object_name::last_ident,
        replay::is_replayable,
    },
    options::TranslationContext,
    traits::schema::function_lookup_target,
    warnings::{TranslationWarning, WarningSink},
};

/// Expands catalog SQL calls while borrowing unchanged expressions.
pub(crate) fn expand_expression<'e>(
    expr: &'e Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Cow<'e, Expr>, Error> {
    let expander = Expander::new(schema, options, emit);
    let needed = sqlparser::ast::visit_expressions(expr, |node| {
        let Expr::Function(function) = node else { return ControlFlow::Continue(()) };
        match expander.preflight(function) {
            Ok(true) => ControlFlow::Break(Ok(())),
            Ok(false) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(Err(error)),
        }
    });
    match needed {
        ControlFlow::Continue(()) => Ok(Cow::Borrowed(expr)),
        ControlFlow::Break(Err(error)) => Err(error),
        ControlFlow::Break(Ok(())) => {
            expander.expand_expr(expr, &Scope::new(), &[], &Walk::root()).map(Cow::Owned)
        }
    }
}

/// Expands a catalog SQL call before ordinary expression translation.
pub(crate) fn try_expand_call(
    func: &Function,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    let expander = Expander::new(schema, options, emit);
    expander.expand_call(func, &Scope::new(), &[], &Walk::root())
}

struct Expander<'a> {
    schema: &'a ParserDB,
    options: &'a TranslationContext<'a>,
    emit: RefCell<&'a mut dyn FnMut(TranslationWarning)>,
    counter: Cell<usize>,
    backing_reads: Cell<usize>,
    argument: RefCell<Option<CallerCapture>>,
}

impl<'a> Expander<'a> {
    fn new(
        schema: &'a ParserDB,
        options: &'a TranslationContext<'a>,
        emit: &'a mut dyn FnMut(TranslationWarning),
    ) -> Self {
        Self {
            schema,
            options,
            emit: RefCell::new(emit),
            counter: Cell::new(0),
            backing_reads: Cell::new(0),
            argument: RefCell::new(None),
        }
    }

    fn is_destination_call(&self, function: &Function) -> bool {
        last_ident(&function.name)
            .is_some_and(|name| self.options.declares_user_defined_function(&name.value))
    }

    fn expand_call(
        &self,
        func: &Function,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<Option<Expr>, Error> {
        let Some(sql) = self.resolve_sql_body(func, scope, renames, walk)? else {
            return Ok(None);
        };
        let label = &sql.label;
        let (params, args, caller_refs) =
            self.bind_call_arguments(label, sql.declaration, func, scope, renames, walk)?;
        let callee_name = last_ident(&sql.declaration.name).map(NameKey::from_ident);
        let callee = CalleeIdentity { params: &params, name: callee_name.as_ref() };
        let body = body_expression(sql.body_sql, label, self, sql.standard_path, &callee)?;
        let mut harvested = Vec::new();
        harvest_expr(&body, &mut harvested);

        let effective = if sql.declaration.security_mode() == FunctionSecurity::Definer {
            FunctionSecurity::Definer
        } else {
            walk.security.clone()
        };
        // Nested invokers inherit the effective definer identity.
        let effective_owner = if sql.declaration.security_mode() == FunctionSecurity::Definer {
            sql.declaration.owner(self.schema)?
        } else {
            walk.effective_owner
        };
        let chain = CallChain { declaration: sql.declaration, parent: walk.chain };
        let inner = Walk {
            params,
            name: callee_name,
            strict: matches!(
                sql.declaration.null_input_behavior(),
                FunctionCalledOnNull::Strict | FunctionCalledOnNull::ReturnsNullOnNullInput
            ),
            args,
            caller_refs,
            harvested,
            chain: Some(&chain),
            security: effective.clone(),
            effective_owner,
            standard_path: sql.standard_path,
            label: &sql.label,
        };

        let saved_reads = self.backing_reads.get();
        self.backing_reads.set(0);
        let result = self.expand_expr(&body, &Scope::new(), renames, &inner)?;
        let own_reads = self.backing_reads.get();
        self.backing_reads.set(saved_reads);
        let mut result = cast_to(result, sql.return_type);
        if inner.strict && !inner.args.is_empty() {
            // The NULL check re-emits every argument AST, so its bare caller
            // references must keep being tracked by the enclosing capture.
            for argument in &inner.args {
                if argument.query_owned {
                    argument.root_use.set(true);
                }
                for key in &argument.unqualified {
                    self.record_unqualified_reference(key);
                }
            }
            let check =
                or_chain(inner.args.iter().map(|arg| Expr::IsNull(Box::new(arg.expr.clone()))))
                    .unwrap();
            result = case_when(
                check,
                Expr::Value(ValueWithSpan { value: Value::Null, span: Span::empty() }),
                Some(result),
            );
        }
        for (position, argument) in inner.args.iter().enumerate() {
            if argument.query_owned && !argument.root_use.get() {
                return Err(refusal(format!(
                    "SQL function {label} cannot omit query-owned argument {}",
                    position + 1
                )));
            }
        }
        if effective == FunctionSecurity::Definer && own_reads > 0 {
            (self.emit.borrow_mut())(TranslationWarning::LossyDrop {
                construct: "SECURITY DEFINER".to_string(),
                reason: format!(
                    "function {label} has no SQLite execution identity and reads locally delivered backing rows under a proven RLS bypass"
                ),
            });
        }
        Ok(Some(result))
    }

    fn resolve_sql_body(
        &self,
        func: &Function,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<Option<SqlFunctionBody<'a>>, Error> {
        if func.uses_odbc_syntax || matches!(func.args, FunctionArguments::None) {
            return Ok(None);
        }
        if self.is_destination_call(func) {
            return Ok(None);
        }
        let callee = CalleeIdentity { params: &walk.params, name: walk.name.as_ref() };
        let CallBinding::Inline(declaration) =
            self.bind_call(func, scope, renames, &callee, walk.standard_path)?
        else {
            return Ok(None);
        };
        let Some(body_sql) = declaration.body() else {
            return Ok(None);
        };
        let label = func.name.to_string();
        if func.filter.is_some()
            || func.over.is_some()
            || !func.within_group.is_empty()
            || func.null_treatment.is_some()
            || !matches!(func.parameters, FunctionArguments::None)
        {
            return Err(refusal(format!(
                "SQL function {label} cannot inline a window, filter, or parameter clause"
            )));
        }
        if walk.chain.is_some_and(|chain| chain.contains(declaration)) {
            return Err(refusal(format!("SQL function {label} is recursive")));
        }
        let standard_path = Self::check_set_params(
            &label,
            declaration.configuration_parameters(),
            walk.standard_path,
        )?;
        if declaration.returns_set() {
            return Err(refusal(format!("SQL function {label} returns a set of rows")));
        }
        let Some(return_type) = declaration.return_type.as_ref().map(|return_type| {
            match return_type {
                FunctionReturnType::DataType(data_type) | FunctionReturnType::SetOf(data_type) => {
                    data_type
                }
            }
        }) else {
            return Err(refusal(format!("SQL function {label} declares no return type")));
        };
        Ok(Some(SqlFunctionBody { declaration, body_sql, label, standard_path, return_type }))
    }

    fn bind_call_arguments(
        &self,
        label: &str,
        declaration: &'a CreateFunction,
        func: &Function,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<BoundCallArguments<'a>, Error> {
        let params = declaration.args.as_deref().unwrap_or_default();
        let mut declared: Vec<Param<'a>> = Vec::with_capacity(params.len());
        for (position, param) in params.iter().enumerate() {
            if let Some(mode) = &param.mode
                && !matches!(mode, ArgMode::In)
            {
                return Err(refusal(format!(
                    "SQL function {label} has unsupported parameter mode {mode} at position {}",
                    position + 1
                )));
            }
            declared.push(Param {
                name: param.name.as_ref().map(NameKey::from_ident),
                data_type: &param.data_type,
                default: param.default_expr.as_ref(),
            });
        }
        let mut supplied: Vec<&Expr> = Vec::new();
        match &func.args {
            FunctionArguments::None => {}
            FunctionArguments::List(list)
                if list.duplicate_treatment.is_none() && list.clauses.is_empty() =>
            {
                for arg in &list.args {
                    let FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) = arg else {
                        return Err(refusal(format!(
                            "SQL function {label} requires positional scalar arguments"
                        )));
                    };
                    supplied.push(expr);
                }
            }
            _ => {
                return Err(refusal(format!(
                    "SQL function {label} requires a scalar argument list"
                )));
            }
        }
        if supplied.len() > declared.len() {
            return Err(refusal(format!(
                "SQL function {label} has {} arguments for {} parameters",
                supplied.len(),
                declared.len()
            )));
        }
        let (args, caller_refs) =
            self.bind_arguments(label, &declared, &supplied, scope, renames, walk)?;
        Ok((declared, args, caller_refs))
    }

    fn check_set_params(
        label: &str,
        params: &[FunctionDefinitionSetParam],
        inherited: bool,
    ) -> Result<bool, Error> {
        let mut standard_path = inherited;
        for param in params {
            let Some(name) = last_ident(&param.name) else {
                return Err(refusal(format!("SQL function {label} has an unnamed local setting")));
            };
            if !name.value.eq_ignore_ascii_case("search_path") {
                return Err(refusal(format!(
                    "SQL function {label} has unsupported local setting {}",
                    name.value
                )));
            }
            match &param.value {
                FunctionSetValue::Default => {
                    return Err(refusal(format!(
                        "SQL function {label} resets `search_path` to an unproven role-dependent default"
                    )));
                }
                FunctionSetValue::FromCurrent => {
                    return Err(refusal(format!(
                        "SQL function {label} has an unproven captured `search_path`"
                    )));
                }
                FunctionSetValue::Values(values) => {
                    standard_path = true;
                    let standard = ["public", "pg_catalog", "pg_temp"];
                    if values.len() != standard.len() {
                        return Err(refusal(format!(
                            "SQL function {label} requires `search_path` to be `public, pg_catalog, pg_temp`"
                        )));
                    }
                    for (position, value) in values.iter().enumerate() {
                        let Expr::Identifier(ident) = value else {
                            return Err(refusal(format!(
                                "SQL function {label} has a non-identifier `search_path` entry"
                            )));
                        };
                        let matches = if ident.quote_style.is_some() {
                            ident.value == standard[position]
                        } else {
                            ident.value.eq_ignore_ascii_case(standard[position])
                        };
                        if !matches {
                            return Err(refusal(format!(
                                "SQL function {label} requires `search_path` to be `public, pg_catalog, pg_temp`"
                            )));
                        }
                    }
                }
            }
        }
        Ok(standard_path)
    }

    fn bind_arguments(
        &self,
        label: &str,
        declared: &[Param<'_>],
        supplied: &[&Expr],
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<(Vec<Argument>, Vec<NameKey>), Error> {
        let mut args = Vec::with_capacity(declared.len());
        for (position, param) in declared.iter().enumerate() {
            let raw = supplied.get(position).copied().or(param.default).ok_or_else(|| {
                refusal(format!(
                    "SQL function {label} is missing argument {} with no declared default",
                    position + 1
                ))
            })?;
            // Expand arguments before entering the callee's parameter scope.
            let mut capture = CallerCapture::new(scope, label);
            let guard = self.enter_argument(&mut capture);
            let expanded = self.expand_expr(raw, scope, renames, walk)?;
            drop(guard);
            if !is_replayable(&expanded, self.options) {
                // Inlining can repeat or omit argument evaluation.
                return Err(refusal(format!(
                    "SQL function {label} cannot preserve one evaluation of argument {}",
                    position + 1
                )));
            }
            let expanded = cast_to(expanded, param.data_type);
            let mut referenced = Vec::new();
            let query_owned = collect_references(&expanded, &mut referenced);
            args.push(Argument {
                expr: expanded,
                referenced,
                unqualified: capture.unqualified,
                query_owned,
                root_use: Cell::new(false),
            });
        }
        let caller_refs = args.iter().flat_map(|arg| arg.referenced.iter().cloned()).collect();
        Ok((args, caller_refs))
    }

    /// Suspends the outer caller capture while one argument expands.
    fn enter_argument<'g>(&'g self, capture: &'g mut CallerCapture) -> ArgumentCapture<'g, 'a> {
        let saved = self.argument.replace(Some(core::mem::take(capture)));
        ArgumentCapture { expander: self, sink: capture, saved }
    }

    fn reference_resolves(&self, reference: &Expr) -> Result<bool, Error> {
        for resolution in self.options.column_definitions(reference, None) {
            match resolution {
                Ok(Some(_)) => return Ok(true),
                Ok(None) => {}
                Err(err) => {
                    return Err(refusal(format!(
                        "Caller reference {reference} cannot be resolved ({err})"
                    )));
                }
            }
        }
        Ok(false)
    }

    fn expand_expr(
        &self,
        expr: &Expr,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<Expr, Error> {
        match expr {
            Expr::CompoundIdentifier(parts) => {
                if let [function, parameter] = parts.as_slice()
                    && let Some(chain) = walk.chain
                    && last_ident(&chain.declaration.name).is_some_and(|name| {
                        NameKey::from_ident(name).matches(&NameKey::from_ident(function))
                    })
                    && let Some(index) = walk.params.iter().position(|param| {
                        param
                            .name
                            .as_ref()
                            .is_some_and(|name| name.matches(&NameKey::from_ident(parameter)))
                    })
                {
                    let source = NameKey::from_ident(function);
                    let effective = renames
                        .iter()
                        .find(|(old, _)| old.matches(&source))
                        .map_or(&source, |(_, fresh)| fresh);
                    if !scope.has_qualified_column(effective, &NameKey::from_ident(parameter)) {
                        return self.substitute(index, scope, walk);
                    }
                }
                let position = if parts.len() == 2 {
                    Some(0)
                } else if parts.len() >= 3 {
                    Some(1)
                } else {
                    None
                };
                if let Some(position) = position {
                    let reference = NameKey::from_ident(&parts[position]);
                    for (old, fresh) in renames {
                        if reference.matches(old) {
                            let mut rewritten = vec![fresh.ident()];
                            if parts.len() == 2 {
                                rewritten.push(parts[1].clone());
                            } else {
                                rewritten.extend_from_slice(&parts[2..]);
                            }
                            return Ok(Expr::CompoundIdentifier(rewritten));
                        }
                    }
                }
                return Ok(expr.clone());
            }
            Expr::Identifier(ident) => {
                if walk.security == FunctionSecurity::Definer
                    && ident.quote_style.is_none()
                    && ident.value.eq_ignore_ascii_case("current_role")
                {
                    return definer_identity(walk);
                }
                let key = NameKey::from_ident(ident);
                // Local columns shadow named parameters.
                if scope.columns.iter().any(|local| key.matches(&local.name)) {
                    return self.expand_bound_reference(ident, expr, &key, scope);
                }
                let Some(index) = walk
                    .params
                    .iter()
                    .position(|param| param.name.as_ref().is_some_and(|name| name.matches(&key)))
                else {
                    return self.expand_unbound_reference(ident, expr, &key);
                };
                return self.substitute(index, scope, walk);
            }
            Expr::Value(ValueWithSpan { value: Value::Placeholder(text), .. }) => {
                // `$N` parameters cannot be shadowed.
                let Some(digits) = text.strip_prefix('$') else {
                    return Ok(expr.clone());
                };
                let Ok(index) = digits.parse::<usize>() else {
                    return Ok(expr.clone());
                };
                if index == 0 || index > walk.params.len() {
                    return Ok(expr.clone());
                }
                return self.substitute(index - 1, scope, walk);
            }
            Expr::Function(func) => {
                if walk.security == FunctionSecurity::Definer
                    && matches!(func.name.0.as_slice(), [ObjectNamePart::Identifier(name)]
                        if name.quote_style.is_none()
                            && ["current_user", "user"].iter().any(|value| name.value.eq_ignore_ascii_case(value)))
                    && matches!(func.args, FunctionArguments::None)
                {
                    return definer_identity(walk);
                }
                if let Some(expanded) = self.expand_call(func, scope, renames, walk)? {
                    return Ok(expanded);
                }
            }
            _ => {}
        }
        try_map_expr_children(
            expr,
            &mut |child| self.expand_expr(child, scope, renames, walk),
            &mut |query| self.expand_query_body(query, scope, renames, walk),
        )
    }

    fn substitute(&self, index: usize, scope: &Scope, walk: &Walk) -> Result<Expr, Error> {
        let argument = &walk.args[index];
        if argument.query_owned {
            if scope.depth > 0 {
                return Err(refusal(format!(
                    "SQL function {} cannot move query-owned argument {} into a subquery",
                    walk.label,
                    index + 1
                )));
            }
            argument.root_use.set(true);
        }
        for key in &argument.unqualified {
            if scope.columns.iter().any(|local| key.matches(&local.name)) {
                return Err(refusal(format!(
                    "SQL function {} argument {} has a captured bare column {}",
                    walk.label,
                    index + 1,
                    key
                )));
            }
        }
        // The cloned argument AST re-expands in the body, so its bare caller
        // references must keep being tracked by the enclosing capture.
        for key in &argument.unqualified {
            self.record_unqualified_reference(key);
        }
        Ok(argument.expr.clone())
    }

    /// Qualifies argument references bound to caller columns.
    fn expand_bound_reference(
        &self,
        ident: &Ident,
        expr: &Expr,
        key: &NameKey,
        scope: &Scope,
    ) -> Result<Expr, Error> {
        let (caller_bound, relation) = {
            let capture = self.argument.borrow();
            match capture.as_ref() {
                // Caller-bound references keep their qualification so body relations cannot capture
                // them.
                Some(capture)
                    if capture.caller.is_prefix_of(scope)
                        && scope.bound_depth(key) <= Some(capture.caller.depth) =>
                {
                    (true, capture.caller.column_relation(key)?.cloned())
                }
                _ => (false, None),
            }
        };
        if !caller_bound {
            return Ok(expr.clone());
        }
        let Some(relation) = relation else {
            self.record_unqualified_reference(key);
            return Ok(expr.clone());
        };
        Ok(Expr::CompoundIdentifier(vec![relation.ident(), ident.clone()]))
    }

    /// Qualifies argument references correlated with caller columns.
    fn expand_unbound_reference(
        &self,
        ident: &Ident,
        expr: &Expr,
        key: &NameKey,
    ) -> Result<Expr, Error> {
        let Some(relation) = self.argument_correlation(key, ident)? else {
            return Ok(expr.clone());
        };
        Ok(Expr::CompoundIdentifier(vec![relation.ident(), ident.clone()]))
    }

    /// Resolves a bare argument reference against the caller scope.
    fn argument_correlation(&self, key: &NameKey, ident: &Ident) -> Result<Option<NameKey>, Error> {
        let caller_relation = {
            let capture = self.argument.borrow();
            match capture.as_ref() {
                Some(capture) => capture.caller.column_relation(key)?.cloned(),
                None => None,
            }
        };
        if let Some(relation) = caller_relation {
            return Ok(Some(relation));
        }
        if self.argument.borrow().is_none() {
            return Ok(None);
        }
        if ident.quote_style.is_none() && self.options.is_variable(&ident.value) {
            self.record_unqualified_reference(key);
            return Ok(None);
        }
        let Some(qualifier) = self.options.resolve_caller_relation(ident)? else {
            self.record_unqualified_reference(key);
            return Ok(None);
        };
        let reference = Expr::CompoundIdentifier(vec![qualifier.clone(), ident.clone()]);
        if !self.reference_resolves(&reference)? {
            let label =
                self.argument.borrow().as_ref().expect("argument capture active").label.clone();
            return Err(refusal(format!(
                "SQL function {label} cannot resolve caller column {} through {}",
                ident.value, qualifier.value
            )));
        }
        Ok(Some(NameKey::from_ident(qualifier)))
    }

    /// Preserves unresolved argument references for capture checks.
    fn record_unqualified_reference(&self, key: &NameKey) {
        let mut argument = self.argument.borrow_mut();
        let Some(capture) = argument.as_mut() else {
            return;
        };
        capture.unqualified.push(key.clone());
    }

    fn expand_query_body(
        &self,
        query: &Query,
        outer: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<Query, Error> {
        let cte_keys: Vec<NameKey> = query
            .with
            .as_ref()
            .map(|with| {
                with.cte_tables.iter().map(|cte| NameKey::from_ident(&cte.alias.name)).collect()
            })
            .unwrap_or_default();
        // Common-table expressions must not capture caller relations.
        let mut cte_renames: Vec<(NameKey, NameKey)> = Vec::new();
        for key in &cte_keys {
            if walk.caller_refs.iter().any(|caller| caller.matches(key)) {
                cte_renames.push((key.clone(), self.fresh_name(key, &cte_renames, walk)));
            }
        }
        let cte_scope = self.cte_scope(outer, query.with.as_ref(), &cte_renames, walk)?;
        let with = query
            .with
            .as_ref()
            .map(|with| self.expand_with(with, &cte_scope, renames, &cte_renames, walk))
            .transpose()?;

        let (body, local, local_renames) =
            self.expand_set_expr(&query.body, &cte_scope, renames, &cte_keys, &cte_renames, walk)?;

        let effective =
            Self::effective_renames(renames, &cte_renames, &local_renames, &local, &cte_keys);
        let mut scope = cte_scope.clone();
        scope.extend_local(&local);

        let order_by = query
            .order_by
            .as_ref()
            .map(|order_by| {
                self.expand_order_by(
                    order_by,
                    projection_items(&query.body),
                    &scope,
                    &effective,
                    walk,
                )
            })
            .transpose()?;
        let limit_clause = query
            .limit_clause
            .as_ref()
            .map(|limit| self.expand_limit_clause(limit, &scope, &effective, walk))
            .transpose()?;

        Ok(Query {
            with,
            body: Box::new(body),
            order_by,
            limit_clause,
            fetch: query
                .fetch
                .as_ref()
                .map(|fetch| {
                    let mut fetch = fetch.clone();
                    fetch.quantity = fetch
                        .quantity
                        .as_ref()
                        .map(|quantity| self.expand_expr(quantity, &scope, &effective, walk))
                        .transpose()?;
                    Ok::<_, Error>(fetch)
                })
                .transpose()?,
            locks: query.locks.clone(),
            for_clause: query.for_clause.clone(),
            settings: query.settings.clone(),
            format_clause: query.format_clause.clone(),
            pipe_operators: query.pipe_operators.clone(),
        })
    }

    fn cte_scope(
        &self,
        outer: &Scope,
        with: Option<&With>,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<Scope, Error> {
        let mut scope = outer.clone();
        if let Some(with) = with {
            for cte in &with.cte_tables {
                let source = NameKey::from_ident(&cte.alias.name);
                let effective = renames
                    .iter()
                    .find(|(old, _)| old.matches(&source))
                    .map_or_else(|| source.clone(), |(_, fresh)| fresh.clone());
                let columns = if cte.alias.columns.is_empty() {
                    self.derived_columns(&cte.query, &scope, walk)?
                } else {
                    cte.alias
                        .columns
                        .iter()
                        .map(|column| NameKey::from_ident(&column.name))
                        .collect()
                };
                scope.ctes.push(CteBinding { source, effective, columns });
            }
        }
        Ok(scope)
    }

    fn effective_renames(
        renames: &[(NameKey, NameKey)],
        cte_renames: &[(NameKey, NameKey)],
        local_renames: &[(NameKey, NameKey)],
        local: &Scope,
        cte_keys: &[NameKey],
    ) -> Vec<(NameKey, NameKey)> {
        renames
            .iter()
            .filter(|(old, _)| {
                !local.relations.iter().any(|key| old.matches(key))
                    && !cte_keys.iter().any(|key| old.matches(key))
            })
            .cloned()
            .chain(cte_renames.iter().cloned())
            .chain(local_renames.iter().cloned())
            .collect()
    }

    fn expand_with(
        &self,
        with: &With,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        cte_renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<With, Error> {
        let effective: Vec<_> =
            renames.iter().cloned().chain(cte_renames.iter().cloned()).collect();
        let inherited_ctes = scope.ctes.len() - with.cte_tables.len();
        let cte_tables = with
            .cte_tables
            .iter()
            .enumerate()
            .map(|(position, cte)| {
                let mut visible = scope.clone();
                if !with.recursive {
                    visible.ctes.truncate(inherited_ctes + position);
                }
                let query = self.expand_query_body(&cte.query, &visible, &effective, walk)?;
                let mut alias = cte.alias.clone();
                let key = NameKey::from_ident(&cte.alias.name);
                if let Some((_, fresh)) = cte_renames.iter().find(|(old, _)| key.matches(old)) {
                    alias.name = fresh.ident();
                }
                Ok(Cte {
                    alias,
                    query: Box::new(query),
                    from: cte.from.clone(),
                    materialized: cte.materialized,
                    closing_paren_token: cte.closing_paren_token.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(With { with_token: with.with_token.clone(), recursive: with.recursive, cte_tables })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive `SetExpr` mapping whose `Select` arm rebuilds the whole struct and would scatter into helpers"
    )]
    fn expand_set_expr(
        &self,
        set_expr: &SetExpr,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        cte_keys: &[NameKey],
        cte_renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<ExpandedSetExpr, Error> {
        match set_expr {
            SetExpr::Select(select) => {
                let (from, own_renames, local) =
                    self.bind_from(&select.from, scope, renames, cte_renames, walk)?;
                let effective =
                    Self::effective_renames(renames, cte_renames, &own_renames, &local, cte_keys);
                let mut select_scope = scope.clone();
                select_scope.extend_local(&local);

                let projection = select
                    .projection
                    .iter()
                    .map(|item| self.expand_select_item(item, &select_scope, &effective, walk))
                    .collect::<Result<Vec<_>, Error>>()?;
                let distinct = select
                    .distinct
                    .as_ref()
                    .map(|distinct| {
                        match distinct {
                            Distinct::On(expressions) => {
                                expressions
                                    .iter()
                                    .map(|expr| {
                                        self.expand_output_expr(
                                            expr,
                                            &select.projection,
                                            &select_scope,
                                            &effective,
                                            walk,
                                        )
                                    })
                                    .collect::<Result<Vec<_>, Error>>()
                                    .map(Distinct::On)
                            }
                            Distinct::Distinct => Ok(Distinct::Distinct),
                            Distinct::All => Ok(Distinct::All),
                        }
                    })
                    .transpose()?;
                let expand_opt = |expr: &Option<Expr>| {
                    expr.as_ref()
                        .map(|expr| self.expand_expr(expr, &select_scope, &effective, walk))
                        .transpose()
                };
                let selection = expand_opt(&select.selection)?;
                let having = expand_opt(&select.having)?;
                let prewhere = expand_opt(&select.prewhere)?;
                let qualify = expand_opt(&select.qualify)?;
                let group_by = self.expand_group_by(
                    &select.group_by,
                    &select.projection,
                    &select_scope,
                    &effective,
                    walk,
                )?;
                let sort_by = select
                    .sort_by
                    .iter()
                    .map(|expr| {
                        self.expand_order_by_expr(expr, &[], &select_scope, &effective, walk)
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                let map_exprs = |exprs: &[Expr]| {
                    exprs
                        .iter()
                        .map(|expr| self.expand_expr(expr, &select_scope, &effective, walk))
                        .collect::<Result<Vec<_>, Error>>()
                };
                let cluster_by = map_exprs(&select.cluster_by)?;
                let distribute_by = map_exprs(&select.distribute_by)?;
                let named_window = select
                    .named_window
                    .iter()
                    .map(|window| self.expand_named_window(window, &select_scope, &effective, walk))
                    .collect::<Result<Vec<_>, Error>>()?;

                // Bind relations before expanding join predicates.
                let mut from = from;
                for table_with_joins in &mut from {
                    for join in &mut table_with_joins.joins {
                        map_join_constraint(&mut join.join_operator, &mut |expr| {
                            self.expand_expr(expr, &select_scope, &effective, walk)
                        })?;
                    }
                }

                Ok((
                    SetExpr::Select(Box::new(Select {
                        select_token: select.select_token.clone(),
                        optimizer_hints: select.optimizer_hints.clone(),
                        distinct,
                        select_modifiers: select.select_modifiers.clone(),
                        top: select.top.clone(),
                        top_before_distinct: select.top_before_distinct,
                        projection,
                        exclude: select.exclude.clone(),
                        into: select.into.clone(),
                        from,
                        lateral_views: select.lateral_views.clone(),
                        prewhere,
                        selection,
                        connect_by: select.connect_by.clone(),
                        group_by,
                        cluster_by,
                        distribute_by,
                        sort_by,
                        having,
                        named_window,
                        qualify,
                        window_before_qualify: select.window_before_qualify,
                        value_table_mode: select.value_table_mode,
                        flavor: select.flavor,
                    })),
                    local,
                    own_renames,
                ))
            }
            SetExpr::Query(inner) => {
                let expanded = self.expand_query_body(inner, scope, renames, walk)?;
                let mut local = Scope::new();
                for key in projection_names(&expanded.body) {
                    local.bind_column(key);
                }
                Ok((SetExpr::Query(Box::new(expanded)), local, Vec::new()))
            }
            SetExpr::SetOperation { left, op, set_quantifier, right } => {
                let (left, _, _) =
                    self.expand_set_expr(left, scope, renames, cte_keys, cte_renames, walk)?;
                let (right, _, _) =
                    self.expand_set_expr(right, scope, renames, cte_keys, cte_renames, walk)?;
                let mut local = Scope::new();
                for key in projection_names(&left) {
                    local.bind_column(key);
                }
                Ok((
                    SetExpr::SetOperation {
                        left: Box::new(left),
                        op: *op,
                        set_quantifier: *set_quantifier,
                        right: Box::new(right),
                    },
                    local,
                    Vec::new(),
                ))
            }
            SetExpr::Values(values) => {
                let mut values_scope = scope.clone();
                values_scope.depth += 1;
                let rows = values
                    .rows
                    .iter()
                    .map(|row| {
                        let content = row
                            .content
                            .iter()
                            .map(|expr| self.expand_expr(expr, &values_scope, renames, walk))
                            .collect::<Result<Vec<_>, Error>>()?;
                        Ok(Parens {
                            opening_token: row.opening_token.clone(),
                            content,
                            closing_token: row.closing_token.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Ok((
                    SetExpr::Values(sqlparser::ast::Values {
                        explicit_row: values.explicit_row,
                        value_keyword: values.value_keyword,
                        rows,
                    }),
                    Scope::new(),
                    Vec::new(),
                ))
            }
            SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => {
                Err(refusal(format!(
                    "SQL function {} contains a data-modifying body statement",
                    walk.label
                )))
            }
            SetExpr::Table(table) => Ok((SetExpr::Table(table.clone()), Scope::new(), Vec::new())),
        }
    }

    fn bind_from(
        &self,
        from: &[TableWithJoins],
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        cte_renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<ExpandedFrom, Error> {
        let mut local = Scope::new();
        let mut own_renames: Vec<(NameKey, NameKey)> = Vec::new();
        let mut rebuilt: Vec<TableWithJoins> = Vec::with_capacity(from.len());
        for table_with_joins in from {
            let relation = self.bind_factor(
                &table_with_joins.relation,
                &mut local,
                &mut own_renames,
                scope,
                renames,
                cte_renames,
                walk,
            )?;
            let mut joins = Vec::with_capacity(table_with_joins.joins.len());
            for join in &table_with_joins.joins {
                let relation = self.bind_factor(
                    &join.relation,
                    &mut local,
                    &mut own_renames,
                    scope,
                    renames,
                    cte_renames,
                    walk,
                )?;
                joins.push(Join {
                    relation,
                    global: join.global,
                    join_operator: join.join_operator.clone(),
                });
            }
            rebuilt.push(TableWithJoins { relation, joins });
        }
        Ok((rebuilt, own_renames, local))
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "exhaustive `TableFactor` mapping that threads scope and caller-rename state through the alias, CTE, and backing-table branches"
    )]
    fn bind_factor(
        &self,
        factor: &TableFactor,
        local: &mut Scope,
        own_renames: &mut Vec<(NameKey, NameKey)>,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        cte_renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<TableFactor, Error> {
        match factor {
            TableFactor::Table {
                name,
                alias,
                args,
                with_hints,
                version,
                with_ordinality,
                partitions,
                json_path,
                sample,
                index_hints,
            } => {
                if args.is_some()
                    || !with_hints.is_empty()
                    || version.is_some()
                    || *with_ordinality
                    || !partitions.is_empty()
                    || json_path.is_some()
                    || sample.is_some()
                    || !index_hints.is_empty()
                {
                    return Err(refusal(format!(
                        "SQL function {} cannot inline relation arguments or options",
                        walk.label
                    )));
                }
                let declared = alias
                    .as_ref()
                    .map(|alias| NameKey::from_ident(&alias.name))
                    .or_else(|| last_ident(name).map(NameKey::from_ident));
                let mut alias_out = alias.clone();
                if let Some(key) = &declared
                    && walk.caller_refs.iter().any(|caller| caller.matches(key))
                {
                    let fresh = self.fresh_name(key, own_renames, walk);
                    own_renames.push((key.clone(), fresh.clone()));
                    if let Some(alias) = alias_out.as_mut() {
                        alias.name = fresh.ident();
                    } else {
                        alias_out = Some(TableAlias {
                            explicit: true,
                            name: fresh.ident(),
                            columns: Vec::new(),
                            at: None,
                        });
                    }
                }
                let bound = alias_out
                    .as_ref()
                    .map(|alias| NameKey::from_ident(&alias.name))
                    .or_else(|| declared.clone());
                if let Some(key) = &bound {
                    local.bind_relation(key.clone());
                }
                let column_start = local.columns.len();
                if let Some(alias) = &alias_out {
                    for column in &alias.columns {
                        local.bind_column(NameKey::from_ident(&column.name));
                    }
                    if let Some(at) = &alias.at {
                        local.bind_column(NameKey::from_ident(at));
                    }
                }
                let cte = scope.cte(name);
                if let Some(cte) = cte {
                    let renamed_columns = alias_out.as_ref().map_or(0, |alias| alias.columns.len());
                    for column in cte.columns.iter().skip(renamed_columns) {
                        local.bind_column(column.clone());
                    }
                    if alias_out.is_none()
                        && !cte.source.matches(&cte.effective)
                        && let Some(relation) = local.relations.last_mut()
                    {
                        *relation = cte.effective.clone();
                    }
                    return Ok(TableFactor::Table {
                        name: ObjectName::from(vec![cte.effective.ident()]),
                        alias: alias_out,
                        args: None,
                        with_hints: Vec::new(),
                        version: None,
                        with_ordinality: false,
                        partitions: Vec::new(),
                        json_path: None,
                        sample: None,
                        index_hints: Vec::new(),
                    });
                }
                let table = self.resolve_table(name, walk.standard_path)?;
                if let Some(table) = table {
                    let renamed_columns = alias_out.as_ref().map_or(0, |alias| alias.columns.len());
                    for (index, column) in table.columns(self.schema)?.enumerate() {
                        let attribute = column.attribute();
                        let class = class_of(&attribute.data_type);
                        if index < renamed_columns {
                            local.columns[column_start + index].class = class;
                        } else {
                            local.bind_typed_column(NameKey::from_ident(&attribute.name), class);
                        }
                    }
                    let mut name_out = table.name.clone();
                    if name_out.0.len() == 1 {
                        name_out.0.insert(0, ObjectNamePart::Identifier(Ident::new("public")));
                    }
                    // Backing reads require the definer's proven PostgreSQL
                    // exemption.
                    if walk.security == FunctionSecurity::Definer
                        && table.has_row_level_security(self.schema)?
                    {
                        match self.definer_rls_bypass(walk.effective_owner, table)? {
                            RlsBypass::Proven => {
                                self.backing_reads.set(self.backing_reads.get() + 1);
                                name_out = backing_name(name, self.options.get_rls_table_suffix());
                                if alias_out.is_none() {
                                    alias_out = last_ident(name).map(|ident| {
                                        TableAlias {
                                            explicit: true,
                                            name: ident.clone(),
                                            columns: Vec::new(),
                                            at: None,
                                        }
                                    });
                                }
                            }
                            RlsBypass::NotOwner => {
                                let owner = walk.effective_owner.unwrap_or("(no declared owner)");
                                return Err(refusal(format!(
                                    "SQL function {} owner {} lacks a proven row-security exemption on {}",
                                    walk.label,
                                    owner,
                                    table.table_name()
                                )));
                            }
                            RlsBypass::Forced => {
                                return Err(refusal(format!(
                                    "SQL function {} lacks a proven exemption from forced row security on {}",
                                    walk.label,
                                    table.table_name()
                                )));
                            }
                        }
                    }
                    return Ok(TableFactor::Table {
                        name: name_out,
                        alias: alias_out,
                        args: None,
                        with_hints: Vec::new(),
                        version: None,
                        with_ordinality: false,
                        partitions: Vec::new(),
                        json_path: None,
                        sample: None,
                        index_hints: Vec::new(),
                    });
                }
                Err(refusal(format!(
                    "SQL function {} cannot resolve body relation {name}",
                    walk.label
                )))
            }
            TableFactor::Derived { lateral, subquery, alias, sample } => {
                if *lateral || sample.is_some() {
                    return Err(refusal(format!(
                        "SQL function {} cannot inline a lateral or sampled body subquery",
                        walk.label
                    )));
                }
                let mut alias_out = alias.clone();
                if let Some(alias) = alias_out.as_mut() {
                    let key = NameKey::from_ident(&alias.name);
                    if walk.caller_refs.iter().any(|caller| caller.matches(&key)) {
                        let fresh = self.fresh_name(&key, own_renames, walk);
                        own_renames.push((key.clone(), fresh.clone()));
                        alias.name = fresh.ident();
                    }
                    local.bind_relation(NameKey::from_ident(&alias.name));
                }
                let renamed_columns = alias_out.as_ref().map_or(0, |alias| {
                    for column in &alias.columns {
                        local.bind_column(NameKey::from_ident(&column.name));
                    }
                    alias.columns.len()
                });
                for key in
                    self.derived_columns(subquery, scope, walk)?.into_iter().skip(renamed_columns)
                {
                    local.bind_column(key);
                }
                // Non-lateral subqueries cannot see sibling relation bindings.
                let effective: Vec<_> =
                    renames.iter().cloned().chain(cte_renames.iter().cloned()).collect();
                let subquery = Box::new(self.expand_query_body(subquery, scope, &effective, walk)?);
                Ok(TableFactor::Derived {
                    lateral: false,
                    subquery,
                    alias: alias_out,
                    sample: None,
                })
            }
            _ => {
                Err(refusal(format!(
                    "SQL function {} requires plain tables or derived body relations",
                    walk.label
                )))
            }
        }
    }

    fn derived_columns(
        &self,
        query: &Query,
        scope: &Scope,
        walk: &Walk,
    ) -> Result<Vec<NameKey>, Error> {
        self.projected_columns(&query.body, scope, walk)
    }

    fn projected_columns(
        &self,
        body: &SetExpr,
        scope: &Scope,
        walk: &Walk,
    ) -> Result<Vec<NameKey>, Error> {
        let select = match body {
            SetExpr::Select(select) => select,
            SetExpr::Query(query) => return self.derived_columns(query, scope, walk),
            SetExpr::SetOperation { left, .. } => return self.projected_columns(left, scope, walk),
            _ => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        let mut wildcard = false;
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => wildcard = true,
                SelectItem::ExprWithAlias { alias, .. } => {
                    out.push(NameKey::from_ident(alias));
                }
                SelectItem::ExprWithAliases { aliases, .. } => {
                    for alias in aliases {
                        out.push(NameKey::from_ident(alias));
                    }
                }
                SelectItem::UnnamedExpr(expr) => {
                    if let Some(label) = implicit_label(expr) {
                        out.push(NameKey::from_ident(label));
                    }
                }
            }
        }
        if !wildcard {
            return Ok(out);
        }
        for table_with_joins in &select.from {
            out.extend(self.factor_columns(&table_with_joins.relation, scope, walk)?);
            for join in &table_with_joins.joins {
                out.extend(self.factor_columns(&join.relation, scope, walk)?);
            }
        }
        Ok(out)
    }

    fn factor_columns(
        &self,
        factor: &TableFactor,
        scope: &Scope,
        walk: &Walk,
    ) -> Result<Vec<NameKey>, Error> {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                if let Some(alias) = alias
                    && !alias.columns.is_empty()
                {
                    return Ok(alias
                        .columns
                        .iter()
                        .map(|column| NameKey::from_ident(&column.name))
                        .collect());
                }
                if let Some(cte) = scope.cte(name) {
                    return Ok(cte.columns.clone());
                }
                let Some(table) = self.resolve_table(name, walk.standard_path)? else {
                    return Ok(Vec::new());
                };
                let mut out = Vec::new();
                for column in table.columns(self.schema)? {
                    out.push(NameKey::from_ident(&column.attribute().name));
                }
                Ok(out)
            }
            TableFactor::Derived { subquery, .. } => self.derived_columns(subquery, scope, walk),
            _ => Ok(Vec::new()),
        }
    }

    /// Checks whether a catalog SQL call requires expansion.
    fn preflight(&self, func: &Function) -> Result<bool, Error> {
        if func.uses_odbc_syntax
            || matches!(func.args, FunctionArguments::None)
            || self.is_destination_call(func)
        {
            return Ok(false);
        }
        let Some(target) = function_lookup_target(&func.name) else {
            return Ok(false);
        };
        if target.schema().is_some() {
            let needed = self
                .schema
                .resolve_target_function(target, IdentifierCase::AsWritten)?
                .is_some_and(|declaration| {
                    !is_internal_stub(declaration) && is_sql_body(declaration)
                });
            return Ok(needed);
        }
        if !crate::impls::sqlite_functions::postgres_has_name(
            target.name(),
            target.name_is_quoted(),
        ) {
            let mut needed = false;
            self.for_each_search_path(false, |schema, quoted| {
                let Some(declaration) = self
                    .schema
                    .function_by_target(
                        TargetName::new(target.name(), target.name_is_quoted())
                            .with_schema(schema, quoted),
                        IdentifierCase::AsWritten,
                    )?
                    .filter(|declaration| !is_internal_stub(declaration))
                else {
                    return Ok(None);
                };
                if !is_sql_body(declaration) {
                    return Ok(None);
                }
                needed = true;
                Ok(Some(()))
            })?;
            return Ok(needed);
        }
        let Some(args) = positional_arguments(func) else {
            if self.source_in_path(&target, false)? {
                return Err(shadow_refusal(&func.name.to_string()));
            }
            return Ok(false);
        };
        let label = func.name.to_string();
        let signature = native_signature(target.name());
        // A proven native overload with a matching arity decides by the
        // argument classes, which the shallow pass cannot read.
        if signature.is_some_and(|signature| signature.len() == args.len()) {
            return Ok(true);
        }
        let implicit =
            !self.schema.search_path().any(|(schema, quoted)| is_pg_catalog(schema, quoted));
        let mut native_first = implicit;
        let found = self.for_each_search_path(false, |schema, quoted| {
            if is_pg_catalog(schema, quoted) {
                native_first = true;
                return Ok(None);
            }
            let Some(declaration) = self
                .schema
                .function_by_target(
                    TargetName::new(target.name(), target.name_is_quoted())
                        .with_schema(schema, quoted),
                    IdentifierCase::AsWritten,
                )?
                .filter(|declaration| !is_internal_stub(declaration))
            else {
                return Ok(None);
            };
            if !accepts_arity(declaration, args.len()) {
                return Ok(None);
            }
            if signature.is_none() && (native_first || !is_sql_body(declaration)) {
                return Err(shadow_refusal(&label));
            }
            Ok(Some(()))
        })?;
        if found.is_some() {
            // A same-arity source settles the call by the argument classes.
            return Ok(true);
        }
        if signature.is_some() {
            return Err(no_overload_refusal(&label));
        }
        Ok(false)
    }

    /// Chooses a SQL body or native call under the effective path.
    fn bind_call(
        &self,
        func: &Function,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        callee: &CalleeIdentity<'_, '_>,
        standard_path: bool,
    ) -> Result<CallBinding<'a>, Error> {
        if self.is_destination_call(func) {
            return Ok(CallBinding::Native);
        }
        let Some(target) = function_lookup_target(&func.name) else {
            return Ok(CallBinding::Native);
        };
        if target.schema().is_some() {
            return self.bind_qualified_call(func, target, scope, renames, callee);
        }
        if !crate::impls::sqlite_functions::postgres_has_name(
            target.name(),
            target.name_is_quoted(),
        ) {
            return self.bind_source_call(func, &target, scope, renames, callee, standard_path);
        }
        self.bind_native_call(func, &target, scope, renames, callee, standard_path)
    }

    /// Resolves applicable source declarations before selecting a SQL body.
    fn bind_source_call(
        &self,
        func: &Function,
        target: &TargetName<'_>,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        callee: &CalleeIdentity<'_, '_>,
        standard_path: bool,
    ) -> Result<CallBinding<'a>, Error> {
        let label = func.name.to_string();
        let Some(args) = positional_arguments(func) else {
            return Err(refusal(format!(
                "SQL function {label} requires positional scalar arguments"
            )));
        };
        let mut first = None;
        let mut multiple = false;
        let mut declared = false;
        self.for_each_search_path(standard_path, |schema, quoted| {
            let Some(declaration) = self
                .schema
                .function_by_target(
                    TargetName::new(target.name(), target.name_is_quoted())
                        .with_schema(schema, quoted),
                    IdentifierCase::AsWritten,
                )?
                .filter(|declaration| !is_internal_stub(declaration))
            else {
                return Ok(None);
            };
            declared = true;
            if !accepts_arity(declaration, args.len()) {
                return Ok(None);
            }
            if first.is_some() {
                multiple = true;
                return Ok(Some(()));
            }
            first = Some(declaration);
            Ok(None)
        })?;
        let Some(mut selected) = first else {
            return if declared {
                Err(no_overload_refusal(&label))
            } else {
                Ok(CallBinding::Native)
            };
        };
        if multiple {
            selected = self
                .for_each_search_path(standard_path, |schema, quoted| {
                    let Some(declaration) = self
                        .schema
                        .function_by_target(
                            TargetName::new(target.name(), target.name_is_quoted())
                                .with_schema(schema, quoted),
                            IdentifierCase::AsWritten,
                        )?
                        .filter(|declaration| !is_internal_stub(declaration))
                    else {
                        return Ok(None);
                    };
                    if !accepts_arity(declaration, args.len()) {
                        return Ok(None);
                    }
                    let params = declaration.args.as_deref().unwrap_or_default();
                    match self.arguments_fit(&args, params, scope, renames, callee)? {
                        Fit::AllMatch => Ok(Some(declaration)),
                        Fit::Mismatch => Ok(None),
                        Fit::AnyUnknown => {
                            Err(refusal(format!(
                                "SQL function {label} cannot prove its source overload"
                            )))
                        }
                    }
                })?
                .ok_or_else(|| no_overload_refusal(&label))?;
        }
        Ok(if is_sql_body(selected) { CallBinding::Inline(selected) } else { CallBinding::Native })
    }

    /// Proves argument types for a schema-qualified source call.
    fn bind_qualified_call(
        &self,
        func: &Function,
        target: TargetName<'_>,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        callee: &CalleeIdentity<'_, '_>,
    ) -> Result<CallBinding<'a>, Error> {
        let native = crate::impls::sqlite_functions::postgres_has_name(
            target.name(),
            target.name_is_quoted(),
        );
        let Some(declaration) = self
            .schema
            .resolve_target_function(target, IdentifierCase::AsWritten)?
            .filter(|declaration| !is_internal_stub(declaration))
        else {
            return Ok(CallBinding::Native);
        };
        if !is_sql_body(declaration) {
            return Ok(CallBinding::Native);
        }
        if !native {
            return Ok(CallBinding::Inline(declaration));
        }
        let label = func.name.to_string();
        let Some(args) = positional_arguments(func) else {
            return Err(refusal(format!(
                "SQL function {label} requires positional scalar arguments"
            )));
        };
        let params = declaration.args.as_deref().unwrap_or_default();
        if !accepts_arity(declaration, args.len()) {
            return Err(refusal(format!(
                "SQL function {label} has {} arguments for {} parameters",
                args.len(),
                params.len()
            )));
        }
        if matches!(self.arguments_fit(&args, params, scope, renames, callee)?, Fit::AllMatch) {
            return Ok(CallBinding::Inline(declaration));
        }
        Err(refusal(format!(
            "SQL function {label} does not accept the argument types the call supplies"
        )))
    }

    /// Resolves native overloads and source declarations under the effective
    /// path.
    fn bind_native_call(
        &self,
        func: &Function,
        target: &TargetName<'_>,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        callee: &CalleeIdentity<'_, '_>,
        standard_path: bool,
    ) -> Result<CallBinding<'a>, Error> {
        let label = func.name.to_string();
        let Some(args) = positional_arguments(func) else {
            if self.source_in_path(target, standard_path)? {
                return Err(shadow_refusal(&label));
            }
            return Ok(CallBinding::Native);
        };
        let implicit = !standard_path
            && !self.schema.search_path().any(|(schema, quoted)| is_pg_catalog(schema, quoted));
        let signature = native_signature(target.name());
        let (native_applies, proven_non_numeric) =
            if signature.is_some_and(|signature| signature.len() == args.len()) {
                let mut all_numeric = true;
                let mut proven_non_numeric = false;
                for arg in args.iter() {
                    match self.argument_class(arg, scope, renames, callee)? {
                        ArgClass::Integer
                        | ArgClass::SmallInteger
                        | ArgClass::BigInteger
                        | ArgClass::Numeric
                        | ArgClass::Real
                        | ArgClass::Double => {}
                        ArgClass::Text | ArgClass::Character => {
                            all_numeric = false;
                            proven_non_numeric = true;
                        }
                        ArgClass::Unknown => all_numeric = false,
                    }
                }
                (all_numeric, proven_non_numeric)
            } else {
                (false, false)
            };
        if implicit && native_applies {
            // pg_catalog is searched before the declared path, so a proven
            // native overload answers before any same-signature source.
            return Ok(CallBinding::Native);
        }
        let mut native_first = implicit;
        let mut standard =
            ["public", "pg_catalog", "pg_temp"].into_iter().map(|name| (name, false));
        let mut inherited = self.schema.search_path();
        let path: &mut dyn Iterator<Item = (&str, bool)> =
            if standard_path { &mut standard } else { &mut inherited };
        for (schema, quoted) in path {
            if is_pg_catalog(schema, quoted) {
                if native_applies {
                    return Ok(CallBinding::Native);
                }
                native_first = true;
                continue;
            }
            let Some(declaration) = self
                .schema
                .function_by_target(
                    TargetName::new(target.name(), target.name_is_quoted())
                        .with_schema(schema, quoted),
                    IdentifierCase::AsWritten,
                )?
                .filter(|declaration| !is_internal_stub(declaration))
            else {
                continue;
            };
            let params = declaration.args.as_deref().unwrap_or_default();
            if !accepts_arity(declaration, args.len()) {
                continue;
            }
            if (native_first && signature.is_none()) || !is_sql_body(declaration) {
                return Err(shadow_refusal(&label));
            }
            match self.arguments_fit(&args, params, scope, renames, callee)? {
                Fit::AllMatch => return Ok(CallBinding::Inline(declaration)),
                Fit::AnyUnknown => {
                    return Err(refusal(format!(
                        "SQL function {label} cannot bind the argument types PostgreSQL resolves the call with"
                    )));
                }
                Fit::Mismatch => {}
            }
        }
        if native_applies {
            return Ok(CallBinding::Native);
        }
        let Some(signature) = signature else {
            return Ok(CallBinding::Native);
        };
        // With no source applying above, an argument proven non-numeric has
        // no overload the destination accepts, and PostgreSQL errors.
        if signature.len() != args.len() || proven_non_numeric {
            return Err(no_overload_refusal(&label));
        }
        Ok(CallBinding::Native)
    }

    /// Searches path entries until the visitor returns `Ok(Some(value))`.
    fn for_each_search_path<T>(
        &self,
        standard_path: bool,
        mut visit: impl FnMut(&str, bool) -> Result<Option<T>, Error>,
    ) -> Result<Option<T>, Error> {
        let mut standard =
            ["public", "pg_catalog", "pg_temp"].into_iter().map(|name| (name, false));
        let mut inherited = self.schema.search_path();
        let path: &mut dyn Iterator<Item = (&str, bool)> =
            if standard_path { &mut standard } else { &mut inherited };
        for (schema, quoted) in path {
            if let Some(value) = visit(schema, quoted)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Whether a non-stub source of the call's name sits in the path.
    fn source_in_path(&self, target: &TargetName<'_>, standard_path: bool) -> Result<bool, Error> {
        let found = self.for_each_search_path(standard_path, |schema, quoted| {
            let Some(_) = self
                .schema
                .function_by_target(
                    TargetName::new(target.name(), target.name_is_quoted())
                        .with_schema(schema, quoted),
                    IdentifierCase::AsWritten,
                )?
                .filter(|declaration| !is_internal_stub(declaration))
            else {
                return Ok(None);
            };
            Ok(Some(()))
        })?;
        Ok(found.is_some())
    }

    /// Whether the declared parameter types bind the actual arguments.
    fn arguments_fit(
        &self,
        args: &PositionalArguments<'_>,
        params: &[OperateFunctionArg],
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        callee: &CalleeIdentity<'_, '_>,
    ) -> Result<Fit, Error> {
        for (arg, param) in args.iter().zip(params) {
            let declared = class_of(&param.data_type);
            if declared == ArgClass::Unknown {
                return Ok(Fit::AnyUnknown);
            }
            if declared == ArgClass::Character {
                return Ok(Fit::AnyUnknown);
            }
            let actual = self.argument_class(arg, scope, renames, callee)?;
            if actual == ArgClass::Unknown {
                return Ok(Fit::AnyUnknown);
            }
            if actual != declared {
                return Ok(Fit::Mismatch);
            }
        }
        Ok(Fit::AllMatch)
    }

    /// The class a single argument is proven to carry.
    fn argument_class(
        &self,
        expr: &Expr,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        callee: &CalleeIdentity<'_, '_>,
    ) -> Result<ArgClass, Error> {
        let mut bare = expr;
        while let Expr::Nested(inner) = bare {
            bare = &**inner;
        }
        match bare {
            Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } => {
                let mut operand = &**expr;
                while let Expr::Nested(inner) = operand {
                    operand = inner;
                }
                if let Expr::Value(ValueWithSpan { value: Value::Number(digits, ..), .. }) = operand
                {
                    return Ok(number_class(
                        digits,
                        matches!(bare, Expr::UnaryOp { op: UnaryOperator::Minus, .. }),
                    ));
                }
                if matches!(operand, Expr::UnaryOp { .. }) {
                    return Ok(ArgClass::Unknown);
                }
                self.argument_class(expr, scope, renames, callee)
            }
            Expr::Value(ValueWithSpan { value: Value::Number(digits, ..), .. }) => {
                Ok(number_class(digits, false))
            }
            Expr::Cast { data_type, .. } => Ok(class_of(data_type)),
            Expr::Identifier(ident) => {
                let key = NameKey::from_ident(ident);
                let relation = scope.column_relation(&key)?;
                if let Some(relation) = relation {
                    return Ok(scope.column_class(relation, &key));
                }
                if let Some(class) = callee
                    .params
                    .iter()
                    .position(|param| param.name.as_ref().is_some_and(|name| name.matches(&key)))
                    .map(|index| class_of(callee.params[index].data_type))
                {
                    return Ok(class);
                }
                Ok(self.caller_reference_class(bare))
            }
            Expr::CompoundIdentifier(parts) if parts.len() == 2 => {
                let relation = NameKey::from_ident(&parts[0]);
                let column = NameKey::from_ident(&parts[1]);
                let effective = renames
                    .iter()
                    .rev()
                    .find_map(|(old, fresh)| old.matches(&relation).then_some(fresh))
                    .unwrap_or(&relation);
                if scope.has_qualified_column(effective, &column) {
                    return Ok(scope.column_class(effective, &column));
                }
                if callee.name.is_some_and(|name| name.matches(&relation))
                    && let Some(class) = callee
                        .params
                        .iter()
                        .position(|param| {
                            param.name.as_ref().is_some_and(|name| name.matches(&column))
                        })
                        .map(|index| class_of(callee.params[index].data_type))
                {
                    return Ok(class);
                }
                Ok(self.caller_reference_class(bare))
            }
            _ => Ok(ArgClass::Unknown),
        }
    }

    /// The caller-scope declaration of a bare reference, or no proven class.
    fn caller_reference_class(&self, expr: &Expr) -> ArgClass {
        declared_argument_class(expr, self.schema, self.options).unwrap_or(ArgClass::Unknown)
    }

    fn resolve_table(
        &self,
        name: &ObjectName,
        standard_path: bool,
    ) -> Result<Option<&<ParserDB as DatabaseLike>::Table>, Error> {
        let Some(target) = function_lookup_target(name) else {
            return Err(refusal(format!("The relation {name} has an unsupported identity.")));
        };
        if !standard_path || target.schema().is_some() {
            return Ok(self.schema.resolve_table_object_name_on_search_path(name)?);
        }
        for schema in ["public", "pg_catalog", "pg_temp"] {
            let qualified =
                TargetName::new(target.name(), target.name_is_quoted()).with_schema(schema, false);
            if let Some(table) =
                self.schema.table_by_target(qualified, IdentifierCase::AsWritten)?
            {
                return Ok(Some(table));
            }
        }
        Ok(None)
    }

    fn definer_rls_bypass(
        &self,
        owner: Option<&str>,
        table: &<ParserDB as DatabaseLike>::Table,
    ) -> Result<RlsBypass, Error> {
        if let Some(owner_name) = owner
            && let Some(role) = self.schema.role(owner_name)
            && (role.is_superuser() || role.can_bypass_rls())
        {
            return Ok(RlsBypass::Proven);
        }
        if table.has_forced_row_level_security(self.schema)? {
            return Ok(RlsBypass::Forced);
        }
        let Some(table_owner) = table.owner(self.schema)? else {
            // Objects without declared owners share the schema creator.
            return Ok(if owner.is_none() { RlsBypass::Proven } else { RlsBypass::NotOwner });
        };
        match owner {
            Some(owner_name) if owner_name == table_owner => Ok(RlsBypass::Proven),
            _ => Ok(RlsBypass::NotOwner),
        }
    }

    fn fresh_name(&self, key: &NameKey, renames: &[(NameKey, NameKey)], walk: &Walk) -> NameKey {
        let base = &key.value;
        loop {
            let next = self.counter.get() + 1;
            self.counter.set(next);
            let candidate = NameKey::new(format!("{base}_{next}"), key.quoted);
            let taken = walk
                .harvested
                .iter()
                .chain(walk.caller_refs.iter())
                .chain(renames.iter().flat_map(|(old, fresh)| [old, fresh]))
                .chain(walk.params.iter().filter_map(|param| param.name.as_ref()))
                .any(|local| local.matches(&candidate));
            if !taken {
                return candidate;
            }
        }
    }

    fn expand_select_item(
        &self,
        item: &SelectItem,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<SelectItem, Error> {
        match item {
            SelectItem::UnnamedExpr(expr) => {
                let expanded = self.expand_expr(expr, scope, renames, walk)?;
                let implicit = implicit_label(expr);
                if let Some(alias) = implicit
                    && (expanded != *expr
                        || !matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_)))
                {
                    Ok(SelectItem::ExprWithAlias { expr: expanded, alias: alias.clone() })
                } else {
                    Ok(SelectItem::UnnamedExpr(expanded))
                }
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                Ok(SelectItem::ExprWithAlias {
                    expr: self.expand_expr(expr, scope, renames, walk)?,
                    alias: alias.clone(),
                })
            }
            SelectItem::ExprWithAliases { expr, aliases } => {
                Ok(SelectItem::ExprWithAliases {
                    expr: self.expand_expr(expr, scope, renames, walk)?,
                    aliases: aliases.clone(),
                })
            }
            SelectItem::QualifiedWildcard(kind, options) => {
                let kind = match kind {
                    SelectItemQualifiedWildcardKind::ObjectName(name) => {
                        let mut name = name.clone();
                        if let Some(ident) = last_ident(&name).cloned() {
                            let reference = NameKey::from_ident(&ident);
                            for (old, fresh) in renames {
                                if reference.matches(old) {
                                    name.0.pop();
                                    name.0.push(ObjectNamePart::Identifier(fresh.ident()));
                                    break;
                                }
                            }
                        }
                        SelectItemQualifiedWildcardKind::ObjectName(name)
                    }
                    SelectItemQualifiedWildcardKind::Expr(expr) => {
                        SelectItemQualifiedWildcardKind::Expr(
                            self.expand_expr(expr, scope, renames, walk)?,
                        )
                    }
                };
                Ok(SelectItem::QualifiedWildcard(kind, options.clone()))
            }
            SelectItem::Wildcard(options) => Ok(SelectItem::Wildcard(options.clone())),
        }
    }

    fn expand_output_expr(
        &self,
        expr: &Expr,
        projection: &[SelectItem],
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<Expr, Error> {
        let mut bare = expr;
        while let Expr::Nested(inner) = bare {
            bare = inner;
        }
        if let Expr::Identifier(ident) = bare
            && !(ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("current_role"))
            && projection.iter().any(|item| {
                match item {
                    SelectItem::ExprWithAlias { alias, .. } => idents_match(alias, ident),
                    SelectItem::ExprWithAliases { aliases, .. } => {
                        aliases.iter().any(|alias| idents_match(alias, ident))
                    }
                    SelectItem::UnnamedExpr(expr) => {
                        implicit_label(expr).is_some_and(|label| idents_match(label, ident))
                    }
                    _ => false,
                }
            })
        {
            return Ok(expr.clone());
        }
        self.expand_expr(expr, scope, renames, walk)
    }

    fn expand_group_by(
        &self,
        group_by: &GroupByExpr,
        projection: &[SelectItem],
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<GroupByExpr, Error> {
        match group_by {
            GroupByExpr::Expressions(exprs, modifiers) => {
                Ok(GroupByExpr::Expressions(
                    exprs
                        .iter()
                        .map(|expr| self.expand_output_expr(expr, projection, scope, renames, walk))
                        .collect::<Result<Vec<_>, Error>>()?,
                    modifiers.clone(),
                ))
            }
            GroupByExpr::All(modifiers) => Ok(GroupByExpr::All(modifiers.clone())),
        }
    }

    fn expand_order_by(
        &self,
        order_by: &OrderBy,
        projection: &[SelectItem],
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<OrderBy, Error> {
        let kind = match &order_by.kind {
            OrderByKind::Expressions(exprs) => {
                OrderByKind::Expressions(
                    exprs
                        .iter()
                        .map(|expr| {
                            self.expand_order_by_expr(expr, projection, scope, renames, walk)
                        })
                        .collect::<Result<Vec<_>, Error>>()?,
                )
            }
            OrderByKind::All(options) => OrderByKind::All(options.clone()),
        };
        Ok(OrderBy { kind, interpolate: order_by.interpolate.clone() })
    }

    fn expand_order_by_expr(
        &self,
        expr: &OrderByExpr,
        projection: &[SelectItem],
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<OrderByExpr, Error> {
        let with_fill = expr.with_fill.as_ref().map(|fill| -> Result<_, Error> {
            let expand_opt = |option: &Option<Expr>| {
                option.as_ref().map(|expr| self.expand_expr(expr, scope, renames, walk)).transpose()
            };
            Ok(sqlparser::ast::WithFill {
                from: expand_opt(&fill.from)?,
                to: expand_opt(&fill.to)?,
                step: expand_opt(&fill.step)?,
            })
        });
        let with_fill = with_fill.transpose()?;
        Ok(OrderByExpr {
            expr: self.expand_output_expr(&expr.expr, projection, scope, renames, walk)?,
            options: expr.options.clone(),
            with_fill,
        })
    }

    fn expand_limit_clause(
        &self,
        limit: &LimitClause,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<LimitClause, Error> {
        let expand_opt = |expr: &Option<Expr>| {
            expr.as_ref().map(|expr| self.expand_expr(expr, scope, renames, walk)).transpose()
        };
        match limit {
            LimitClause::LimitOffset { limit, offset, limit_by } => {
                Ok(LimitClause::LimitOffset {
                    limit: expand_opt(limit)?,
                    offset: offset
                        .as_ref()
                        .map(|offset| -> Result<_, Error> {
                            Ok(Offset {
                                value: self.expand_expr(&offset.value, scope, renames, walk)?,
                                rows: offset.rows,
                            })
                        })
                        .transpose()?,
                    limit_by: limit_by
                        .iter()
                        .map(|expr| self.expand_expr(expr, scope, renames, walk))
                        .collect::<Result<Vec<_>, Error>>()?,
                })
            }
            LimitClause::OffsetCommaLimit { offset, limit } => {
                Ok(LimitClause::OffsetCommaLimit {
                    offset: self.expand_expr(offset, scope, renames, walk)?,
                    limit: self.expand_expr(limit, scope, renames, walk)?,
                })
            }
        }
    }

    fn expand_named_window(
        &self,
        window: &NamedWindowDefinition,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<NamedWindowDefinition, Error> {
        let inner = match &window.1 {
            NamedWindowExpr::NamedWindow(_) => window.1.clone(),
            NamedWindowExpr::WindowSpec(spec) => {
                NamedWindowExpr::WindowSpec(self.expand_window_spec(spec, scope, renames, walk)?)
            }
        };
        Ok(NamedWindowDefinition(window.0.clone(), inner))
    }

    fn expand_window_spec(
        &self,
        spec: &WindowSpec,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<WindowSpec, Error> {
        let partition_by = spec
            .partition_by
            .iter()
            .map(|expr| self.expand_expr(expr, scope, renames, walk))
            .collect::<Result<Vec<_>, Error>>()?;
        let order_by = spec
            .order_by
            .iter()
            .map(|expr| self.expand_order_by_expr(expr, &[], scope, renames, walk))
            .collect::<Result<Vec<_>, Error>>()?;
        let window_frame = spec
            .window_frame
            .as_ref()
            .map(|frame| self.expand_window_frame(frame, scope, renames, walk))
            .transpose()?;
        Ok(WindowSpec {
            window_name: spec.window_name.clone(),
            partition_by,
            order_by,
            window_frame,
        })
    }

    fn expand_window_frame(
        &self,
        frame: &WindowFrame,
        scope: &Scope,
        renames: &[(NameKey, NameKey)],
        walk: &Walk,
    ) -> Result<WindowFrame, Error> {
        let expand_bound = |bound: &WindowFrameBound| -> Result<_, Error> {
            match bound {
                WindowFrameBound::CurrentRow => Ok(bound.clone()),
                WindowFrameBound::Preceding(inner) => {
                    let bound = inner
                        .as_ref()
                        .map(|expr| self.expand_expr(expr, scope, renames, walk))
                        .transpose()?
                        .map(Box::new);
                    Ok(WindowFrameBound::Preceding(bound))
                }
                WindowFrameBound::Following(inner) => {
                    let bound = inner
                        .as_ref()
                        .map(|expr| self.expand_expr(expr, scope, renames, walk))
                        .transpose()?
                        .map(Box::new);
                    Ok(WindowFrameBound::Following(bound))
                }
            }
        };
        let start_bound = expand_bound(&frame.start_bound)?;
        let end_bound = frame.end_bound.as_ref().map(expand_bound).transpose()?;
        Ok(WindowFrame { units: frame.units, start_bound, end_bound })
    }
}

struct Walk<'a> {
    params: Vec<Param<'a>>,
    name: Option<NameKey>,
    args: Vec<Argument>,
    strict: bool,
    caller_refs: Vec<NameKey>,
    harvested: Vec<NameKey>,
    chain: Option<&'a CallChain<'a>>,
    security: FunctionSecurity,
    effective_owner: Option<&'a str>,
    standard_path: bool,
    label: &'a str,
}

impl Walk<'_> {
    fn root() -> Self {
        Self {
            params: Vec::new(),
            name: None,
            args: Vec::new(),
            strict: false,
            caller_refs: Vec::new(),
            harvested: Vec::new(),
            chain: None,
            security: FunctionSecurity::Invoker,
            effective_owner: None,
            standard_path: false,
            label: "",
        }
    }
}

struct CallChain<'a> {
    declaration: &'a CreateFunction,
    parent: Option<&'a CallChain<'a>>,
}

impl CallChain<'_> {
    fn contains(&self, declaration: &CreateFunction) -> bool {
        let mut next = Some(self);
        while let Some(chain) = next {
            if core::ptr::eq(chain.declaration, declaration) {
                return true;
            }
            next = chain.parent;
        }
        false
    }
}

enum RlsBypass {
    Proven,
    NotOwner,
    Forced,
}

/// A set expression with the scope and renames its relations introduced.
type ExpandedSetExpr = (SetExpr, Scope, Vec<(NameKey, NameKey)>);

/// A body clause's bound relations, their renames, and the resulting scope.
type ExpandedFrom = (Vec<TableWithJoins>, Vec<(NameKey, NameKey)>, Scope);

type BoundCallArguments<'a> = (Vec<Param<'a>>, Vec<Argument>, Vec<NameKey>);

/// A resolved catalog SQL function whose body is ready for inlining.
struct SqlFunctionBody<'a> {
    declaration: &'a CreateFunction,
    body_sql: &'a str,
    label: String,
    standard_path: bool,
    return_type: &'a DataType,
}

struct Param<'a> {
    name: Option<NameKey>,
    data_type: &'a DataType,
    default: Option<&'a Expr>,
}

struct Argument {
    expr: Expr,
    referenced: Vec<NameKey>,
    unqualified: Vec<NameKey>,
    query_owned: bool,
    root_use: Cell<bool>,
}

/// What a call to a function name resolves to before ordinary translation.
enum CallBinding<'a> {
    /// The destination answers the call; nothing inlines.
    Native,
    /// The call inlines the declared `LANGUAGE sql` body.
    Inline(&'a CreateFunction),
}

/// The enclosing function a call's argument types resolve against.
struct CalleeIdentity<'b, 'a> {
    /// The enclosing function's declared parameters.
    params: &'b [Param<'a>],
    /// The enclosing function's name for qualified parameter references.
    name: Option<&'b NameKey>,
}

/// The class a value is proven to carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgClass {
    /// PostgreSQL `int2`.
    SmallInteger,
    /// PostgreSQL `int4`.
    Integer,
    /// PostgreSQL `int8`.
    BigInteger,
    /// PostgreSQL `numeric`.
    Numeric,
    /// PostgreSQL `float4`.
    Real,
    /// PostgreSQL `float8`.
    Double,
    /// `TEXT` exactly.
    Text,
    /// A character type that is not `TEXT`.
    Character,
    /// No literal, cast, or declaration settles the type.
    Unknown,
}

/// Whether the declared parameter types bind the actual arguments.
enum Fit {
    /// Every argument is proven to match its declared parameter.
    AllMatch,
    /// An argument's type cannot be proven.
    AnyUnknown,
    /// A proven argument does not match its declared parameter.
    Mismatch,
}

/// The class a declared data type belongs to.
fn class_of(data_type: &DataType) -> ArgClass {
    match data_type {
        DataType::SmallInt(None) | DataType::Int2(None) => ArgClass::SmallInteger,
        DataType::Int(None) | DataType::Integer(None) | DataType::Int4(None) => ArgClass::Integer,
        DataType::BigInt(None) | DataType::Int8(None) => ArgClass::BigInteger,
        DataType::Numeric(_) | DataType::Decimal(_) | DataType::Dec(_) => ArgClass::Numeric,
        DataType::Real
        | DataType::Float4
        | DataType::Float(sqlparser::ast::ExactNumberInfo::Precision(1..=24)) => ArgClass::Real,
        DataType::DoublePrecision
        | DataType::Float8
        | DataType::Float(
            sqlparser::ast::ExactNumberInfo::None
            | sqlparser::ast::ExactNumberInfo::Precision(25..=53),
        ) => ArgClass::Double,
        DataType::Text => ArgClass::Text,
        DataType::Varchar(_)
        | DataType::Nvarchar(_)
        | DataType::Char(_)
        | DataType::Character(_)
        | DataType::CharVarying(_)
        | DataType::CharacterVarying(_)
        | DataType::Clob(_)
        | DataType::CharacterLargeObject(_)
        | DataType::CharLargeObject(_) => ArgClass::Character,
        _ => ArgClass::Unknown,
    }
}

fn number_class(digits: &str, negative: bool) -> ArgClass {
    let Ok(mut value) = digits.parse::<i128>() else {
        return ArgClass::Numeric;
    };
    if negative {
        let Some(negated) = value.checked_neg() else {
            return ArgClass::Numeric;
        };
        value = negated;
    }
    if i32::try_from(value).is_ok() {
        ArgClass::Integer
    } else if i64::try_from(value).is_ok() {
        ArgClass::BigInteger
    } else {
        ArgClass::Numeric
    }
}

/// The proven native overload the destination answers under `name`.
fn native_signature(name: &str) -> Option<&'static [ArgClass]> {
    name.eq_ignore_ascii_case("abs").then_some(&[ArgClass::Numeric])
}

/// Whether a search path entry is the catalog schema.
fn is_pg_catalog(schema: &str, quoted: bool) -> bool {
    if quoted { schema == "pg_catalog" } else { schema.eq_ignore_ascii_case("pg_catalog") }
}

/// A catalog placeholder for the destination's own implementation.
fn is_internal_stub(declaration: &CreateFunction) -> bool {
    declaration.stored_language().is_some_and(|language| language == "internal")
}

fn is_sql_body(declaration: &CreateFunction) -> bool {
    declaration.stored_language().is_some_and(|language| language == "sql")
        && declaration.body().is_some()
}

/// The positional scalar arguments of a call, if it takes them.
fn positional_arguments(func: &Function) -> Option<PositionalArguments<'_>> {
    let FunctionArguments::List(list) = &func.args else {
        return None;
    };
    if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
        return None;
    }
    for arg in &list.args {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) = arg else {
            return None;
        };
        if matches!(expr, Expr::Wildcard(_)) {
            return None;
        }
    }
    Some(PositionalArguments(&list.args))
}

struct PositionalArguments<'a>(&'a [FunctionArg]);

impl PositionalArguments<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn iter(&self) -> impl Iterator<Item = &Expr> {
        self.0.iter().filter_map(|arg| {
            match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                _ => None,
            }
        })
    }
}

fn accepts_arity(declaration: &CreateFunction, supplied: usize) -> bool {
    declaration
        .args
        .as_deref()
        .unwrap_or_default()
        .get(supplied..)
        .is_some_and(|remaining| remaining.iter().all(|param| param.default_expr.is_some()))
}

/// The class a reference resolves to in the caller's declared scope.
fn declared_argument_class(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<ArgClass, Error> {
    match crate::impls::shared_helpers::declared_in_scope(
        expr,
        schema,
        options,
        |column| Some(class_of(&column.data_type)),
        |expression, schema, options| {
            Ok(Some(declared_argument_class(expression, schema, options)?))
        },
    )? {
        Some(class) => Ok(class),
        None => Ok(ArgClass::Unknown),
    }
}

/// A source body sits where the destination's function would be called from.
fn shadow_refusal(label: &str) -> Error {
    refusal(format!(
        "SQL function {label} shadows a native function, and the translator cannot prove PostgreSQL resolves the call to the source body"
    ))
}

/// Neither the native overload nor any source body applies to the call.
fn no_overload_refusal(label: &str) -> Error {
    refusal(format!(
        "SQL function {label} has no source overload that applies, and the native overload does not take the call's argument types"
    ))
}

#[derive(Default)]
struct CallerCapture {
    /// The scope the argument expression expands against.
    caller: Scope,
    /// The callee label for refusal messages.
    label: String,
    /// The bare caller references that stayed unqualified.
    unqualified: Vec<NameKey>,
}

impl CallerCapture {
    fn new(caller: &Scope, label: &str) -> Self {
        Self { caller: caller.clone(), label: label.to_owned(), unqualified: Vec::new() }
    }
}

/// Suspends the outer caller capture while one argument expression expands.
struct ArgumentCapture<'e, 'a> {
    expander: &'e Expander<'a>,
    sink: &'e mut CallerCapture,
    saved: Option<CallerCapture>,
}

impl Drop for ArgumentCapture<'_, '_> {
    fn drop(&mut self) {
        if let Some(finished) = self.expander.argument.replace(self.saved.take()) {
            *self.sink = finished;
        }
    }
}

fn definer_identity(walk: &Walk<'_>) -> Result<Expr, Error> {
    let owner = walk.effective_owner.ok_or_else(|| {
        refusal(format!(
            "SQL function {} reads an effective-role keyword without a declared definer owner",
            walk.label
        ))
    })?;
    Ok(Expr::Value(Value::SingleQuotedString(owner.to_owned()).into()))
}

/// A PostgreSQL identifier with unquoted ASCII case folding applied.
#[derive(Clone, Debug)]
struct NameKey {
    value: String,
    quoted: bool,
}

impl NameKey {
    fn from_ident(ident: &Ident) -> NameKey {
        if ident.quote_style.is_some() {
            NameKey { value: ident.value.clone(), quoted: true }
        } else {
            NameKey { value: ident.value.to_ascii_lowercase(), quoted: false }
        }
    }

    fn new(value: String, quoted: bool) -> NameKey {
        NameKey { value, quoted }
    }

    fn matches(&self, other: &NameKey) -> bool {
        self.value == other.value
    }

    fn ident(&self) -> Ident {
        if self.quoted {
            Ident::with_quote('"', &self.value)
        } else {
            Ident::new(self.value.clone())
        }
    }
}

impl core::fmt::Display for NameKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.quoted { write!(f, "\"{}\"", self.value) } else { f.write_str(&self.value) }
    }
}

#[derive(Clone, Debug, Default)]
struct Scope {
    relations: Vec<NameKey>,
    columns: Vec<BoundColumn>,
    column_relations: Vec<(usize, core::ops::Range<usize>, usize)>,
    depth: usize,
    ctes: Vec<CteBinding>,
}

#[derive(Clone, Debug)]
struct BoundColumn {
    name: NameKey,
    class: ArgClass,
}

#[derive(Clone, Debug)]
struct CteBinding {
    source: NameKey,
    effective: NameKey,
    columns: Vec<NameKey>,
}

impl Scope {
    fn new() -> Self {
        Self::default()
    }

    fn bind_relation(&mut self, key: NameKey) {
        self.relations.push(key);
        self.column_relations.push((
            self.relations.len() - 1,
            self.columns.len()..self.columns.len(),
            self.depth,
        ));
    }

    fn bind_column(&mut self, key: NameKey) {
        self.bind_typed_column(key, ArgClass::Unknown);
    }

    fn bind_typed_column(&mut self, key: NameKey, class: ArgClass) {
        self.columns.push(BoundColumn { name: key, class });
        if let Some((_, columns, _)) = self.column_relations.last_mut() {
            columns.end = self.columns.len();
        }
    }

    fn extend_local(&mut self, local: &Scope) {
        let relation_offset = self.relations.len();
        let column_offset = self.columns.len();
        self.column_relations.extend(local.column_relations.iter().map(
            |(relation, columns, _)| {
                (
                    relation + relation_offset,
                    columns.start + column_offset..columns.end + column_offset,
                    self.depth + 1,
                )
            },
        ));
        self.depth += 1;
        self.relations.extend(local.relations.iter().cloned());
        self.columns.extend(local.columns.iter().cloned());
    }

    fn has_qualified_column(&self, relation: &NameKey, column: &NameKey) -> bool {
        self.column_relations
            .iter()
            .rev()
            .find(|(index, _, _)| self.relations[*index].matches(relation))
            .is_some_and(|(_, columns, _)| {
                self.columns[columns.clone()].iter().any(|bound| bound.name.matches(column))
            })
    }

    fn column_class(&self, relation: &NameKey, column: &NameKey) -> ArgClass {
        self.column_relations
            .iter()
            .rev()
            .find(|(index, _, _)| self.relations[*index].matches(relation))
            .and_then(|(_, columns, _)| {
                self.columns[columns.clone()].iter().find(|bound| bound.name.matches(column))
            })
            .map_or(ArgClass::Unknown, |bound| bound.class)
    }

    fn column_relation(&self, key: &NameKey) -> Result<Option<&NameKey>, Error> {
        let mut found = None;
        let mut found_depth = None;
        for (relation, columns, depth) in self.column_relations.iter().rev() {
            if found_depth.is_some_and(|found| *depth < found) {
                break;
            }
            if self.columns[columns.clone()].iter().any(|column| key.matches(&column.name)) {
                if found.is_some() {
                    return Err(refusal(format!("The body column {key} is ambiguous.")));
                }
                found = Some(&self.relations[*relation]);
                found_depth = Some(*depth);
            }
        }
        Ok(found)
    }

    /// The depth of the deepest binding of `key`, if any.
    fn bound_depth(&self, key: &NameKey) -> Option<usize> {
        self.column_relations.iter().rev().find_map(|(_, columns, depth)| {
            self.columns[columns.clone()]
                .iter()
                .any(|column| key.matches(&column.name))
                .then_some(*depth)
        })
    }

    /// Checks whether the inner scope extends the caller's bindings.
    fn is_prefix_of(&self, inner: &Scope) -> bool {
        self.column_relations.len() <= inner.column_relations.len()
            && self.relations.len() <= inner.relations.len()
            && self.columns.len() <= inner.columns.len()
            && self
                .relations
                .iter()
                .zip(inner.relations.iter())
                .all(|(outer, inner)| outer.matches(inner))
            && self
                .columns
                .iter()
                .zip(inner.columns.iter())
                .all(|(outer, inner)| outer.name.matches(&inner.name) && outer.class == inner.class)
            && self
                .column_relations
                .iter()
                .zip(inner.column_relations.iter())
                .all(|(outer, inner)| outer == inner)
    }

    fn cte(&self, name: &ObjectName) -> Option<&CteBinding> {
        let [ObjectNamePart::Identifier(ident)] = name.0.as_slice() else {
            return None;
        };
        let key = NameKey::from_ident(ident);
        self.ctes.iter().rev().find(|cte| key.matches(&cte.source) || key.matches(&cte.effective))
    }
}

fn body_expression<'a>(
    body_sql: &str,
    label: &str,
    expander: &Expander<'a>,
    standard_path: bool,
    callee: &CalleeIdentity<'_, 'a>,
) -> Result<Expr, Error> {
    let statements = Parser::parse_sql(&PostgreSqlDialect {}, body_sql)
        .map_err(|err| refusal(format!("SQL function {label} has an unparseable body ({err})")))?;
    let [statement]: [Statement; 1] = statements.try_into().map_err(|_| {
        refusal(format!("SQL function {label} requires exactly one body statement"))
    })?;
    let Statement::Query(query) = statement else {
        return Err(refusal(format!("SQL function {label} requires a `SELECT` body")));
    };
    if query.with.is_some()
        || query.order_by.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.settings.is_some()
        || query.format_clause.is_some()
        || !query.pipe_operators.is_empty()
    {
        return Err(refusal(format!(
            "SQL function {label} has unsupported top-level query clauses"
        )));
    }
    let SetExpr::Select(select) = *query.body else {
        return Err(refusal(format!("SQL function {label} requires a scalar `SELECT` projection")));
    };
    if !select.from.is_empty()
        || select.selection.is_some()
        || select.having.is_some()
        || select.prewhere.is_some()
        || select.qualify.is_some()
        || select.into.is_some()
        || select.exclude.is_some()
        || select.distinct.is_some()
        || select.top.is_some()
        || select.top_before_distinct
        || !select.optimizer_hints.is_empty()
        || !select.lateral_views.is_empty()
        || !select.connect_by.is_empty()
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || !select.named_window.is_empty()
        || select.value_table_mode.is_some()
        || !matches!(select.flavor, SelectFlavor::Standard)
    {
        return Err(refusal(format!("SQL function {label} has row-affecting top-level clauses")));
    }
    let GroupByExpr::Expressions(exprs, modifiers) = &select.group_by else {
        return Err(refusal(format!("SQL function {label} has top-level grouping")));
    };
    if !exprs.is_empty() || !modifiers.is_empty() {
        return Err(refusal(format!("SQL function {label} has top-level grouping")));
    }
    let [item]: [SelectItem; 1] = select.projection.try_into().map_err(|_| {
        refusal(format!("SQL function {label} requires exactly one projected expression"))
    })?;
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
            if let ControlFlow::Break(error) = expr.visit(&mut QueryOwnership {
                depth: 0,
                expander,
                standard_path,
                label,
                callee,
                scope: Scope::default(),
            }) {
                return Err(error);
            }
            Ok(expr)
        }
        _ => Err(refusal(format!("SQL function {label} requires a scalar projected expression"))),
    }
}

struct QueryOwnership<'e, 's> {
    depth: usize,
    expander: &'e Expander<'s>,
    standard_path: bool,
    label: &'e str,
    callee: &'e CalleeIdentity<'e, 's>,
    scope: Scope,
}

impl Visitor for QueryOwnership<'_, '_> {
    type Break = Error;

    fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<Self::Break> {
        if !self.expander.options.allows_function_queries() {
            return ControlFlow::Break(refusal(format!(
                "SQL function {} cannot preserve query-body semantics at this call site",
                self.label
            )));
        }
        self.depth += 1;
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _: &Query) -> ControlFlow<Self::Break> {
        self.depth -= 1;
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if self.depth == 0
            && let Expr::Function(function) = expr
        {
            let aggregate = is_aggregate(function);
            if function.over.is_some()
                || function.filter.is_some()
                || !function.within_group.is_empty()
                || (aggregate
                    && match self.expander.bind_call(
                        function,
                        &self.scope,
                        &[],
                        self.callee,
                        self.standard_path,
                    ) {
                        Ok(CallBinding::Native) => true,
                        Ok(CallBinding::Inline(_)) => false,
                        Err(error) => return ControlFlow::Break(error),
                    })
            {
                return ControlFlow::Break(refusal(format!(
                    "SQL function {} has a top-level aggregate or window",
                    self.label
                )));
            }
        }
        ControlFlow::Continue(())
    }
}

fn is_aggregate(function: &Function) -> bool {
    last_ident(&function.name).is_some_and(|ident| {
        [
            "sum",
            "count",
            "avg",
            "min",
            "max",
            "total",
            "group_concat",
            "string_agg",
            "json_group_array",
            "json_group_object",
            "array_agg",
            "bool_and",
            "bool_or",
            "every",
            "json_agg",
            "jsonb_agg",
            "json_object_agg",
            "jsonb_object_agg",
            "bit_and",
            "bit_or",
            "stddev",
            "stddev_pop",
            "stddev_samp",
            "variance",
            "var_pop",
            "var_samp",
            "corr",
            "covar_pop",
            "covar_samp",
            "percentile_cont",
            "percentile_disc",
            "mode",
            "regr_slope",
            "regr_intercept",
            "regr_r2",
            "regr_avgx",
            "regr_avgy",
            "regr_sxx",
            "regr_syy",
            "regr_sxy",
            "regr_count",
            "xmlagg",
            "range_agg",
            "multirange_agg",
        ]
        .iter()
        .any(|name| {
            if ident.quote_style.is_some() {
                ident.value == *name
            } else {
                ident.value.eq_ignore_ascii_case(name)
            }
        })
    })
}

fn collect_references(expr: &Expr, qualifiers: &mut Vec<NameKey>) -> bool {
    let mut query_owned = false;
    let _: ControlFlow<()> = sqlparser::ast::visit_expressions(expr, |node| {
        match node {
            Expr::CompoundIdentifier(parts) => {
                for ident in &parts[..parts.len().saturating_sub(1)] {
                    qualifiers.push(NameKey::from_ident(ident));
                }
            }
            Expr::Function(function) if !query_owned => {
                query_owned = function.over.is_some()
                    || function.filter.is_some()
                    || !function.within_group.is_empty()
                    || is_aggregate(function);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    });
    query_owned
}

fn map_join_constraint<E>(
    join_operator: &mut JoinOperator,
    f: &mut impl FnMut(&Expr) -> Result<Expr, E>,
) -> Result<(), E> {
    let mut rewrite = |constraint: &mut JoinConstraint| -> Result<(), E> {
        if let JoinConstraint::On(expr) = constraint {
            *expr = f(expr)?;
        }
        Ok(())
    };
    match join_operator {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::CrossJoin(constraint)
        | JoinOperator::Semi(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::Anti(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::RightAnti(constraint)
        | JoinOperator::StraightJoin(constraint)
        | JoinOperator::AsOf { constraint, .. } => rewrite(constraint),
        _ => Ok(()),
    }
}

fn implicit_label(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Identifier(ident) => Some(ident),
        Expr::CompoundIdentifier(parts) => parts.last(),
        Expr::Function(function) => last_ident(&function.name),
        Expr::Nested(inner) | Expr::Cast { expr: inner, .. } => implicit_label(inner),
        _ => None,
    }
}

fn idents_match(left: &Ident, right: &Ident) -> bool {
    let folded = |ident: &Ident, byte: u8| {
        if ident.quote_style.is_some() { byte } else { byte.to_ascii_lowercase() }
    };
    left.value
        .bytes()
        .map(|byte| folded(left, byte))
        .eq(right.value.bytes().map(|byte| folded(right, byte)))
}

fn projection_items(set_expr: &SetExpr) -> &[SelectItem] {
    match set_expr {
        SetExpr::Select(select) => &select.projection,
        SetExpr::Query(inner) => projection_items(&inner.body),
        SetExpr::SetOperation { left, .. } => projection_items(left),
        _ => &[],
    }
}

fn projection_names(set_expr: &SetExpr) -> Vec<NameKey> {
    let mut out = Vec::new();
    for item in projection_items(set_expr) {
        match item {
            SelectItem::ExprWithAlias { alias, .. } => out.push(NameKey::from_ident(alias)),
            SelectItem::ExprWithAliases { aliases, .. } => {
                out.extend(aliases.iter().map(NameKey::from_ident));
            }
            SelectItem::UnnamedExpr(expr) => {
                if let Some(label) = implicit_label(expr) {
                    out.push(NameKey::from_ident(label));
                }
            }
            _ => {}
        }
    }
    out
}

struct BindingNames<'a>(&'a mut Vec<NameKey>);

impl Visitor for BindingNames<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if let Some(with) = &query.with {
            self.0.extend(with.cte_tables.iter().map(|cte| NameKey::from_ident(&cte.alias.name)));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        let name = match factor {
            TableFactor::Table { name, alias, .. } => {
                alias.as_ref().map(|alias| &alias.name).or_else(|| last_ident(name))
            }
            TableFactor::Derived { alias, .. } => alias.as_ref().map(|alias| &alias.name),
            _ => None,
        };
        if let Some(name) = name {
            self.0.push(NameKey::from_ident(name));
        }
        ControlFlow::Continue(())
    }
}

fn harvest_expr(expr: &Expr, out: &mut Vec<NameKey>) {
    let _: ControlFlow<()> = expr.visit(&mut BindingNames(out));
}

fn backing_name(name: &ObjectName, suffix: &str) -> ObjectName {
    let Some(ident) = last_ident(name) else {
        return name.clone();
    };
    ObjectName(vec![ObjectNamePart::Identifier(Ident::new(format!("{}{}", ident.value, suffix)))])
}

fn refusal(message: String) -> Error {
    Error::forward_refusal(message)
}

fn or_chain(exprs: impl IntoIterator<Item = Expr>) -> Option<Expr> {
    let mut exprs = exprs.into_iter();
    let first = exprs.next()?;
    Some(exprs.fold(first, |left, right| {
        Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::Or, right: Box::new(right) }
    }))
}

fn cast_to(expr: Expr, data_type: &DataType) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr),
        data_type: data_type.clone(),
        format: None,
    }
}
