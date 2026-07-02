use std::collections::BTreeSet;
use std::fmt::Write;

use wit_parser::{
    Function, Resolve, Result_, Type, TypeDef, TypeDefKind, TypeId, TypeOwner, WorldId, WorldItem,
    WorldKey,
};

use crate::naming::{to_camel_case, to_pascal_case};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum FieldKind {
    Query,
    Mutation,
}

pub(crate) const MUTATION_PREFIXES: &[&str] = &[
    "create", "update", "delete", "remove", "fork", "push", "add", "merge", "set",
];

pub(crate) const MUTATION_SUFFIXES: &[&str] = &["write"];

pub(crate) fn classify(func_name: &str) -> FieldKind {
    let head = func_name.split('-').next().unwrap_or("");
    if MUTATION_PREFIXES.contains(&head) {
        return FieldKind::Mutation;
    }
    let tail = func_name.rsplit('-').next().unwrap_or("");
    if MUTATION_SUFFIXES.contains(&tail) {
        return FieldKind::Mutation;
    }
    FieldKind::Query
}

pub fn generate(resolve: &Resolve, world: WorldId, source_label: &str) -> anyhow::Result<String> {
    let mut gen = Gen::new(resolve);
    gen.collect_world(world);
    gen.render(source_label)
}

struct FieldSdl {
    name: String,
    args: Vec<(String, String)>,
    return_ty: String,
}

struct ResultWrapper {
    pascal: String,
    ok_ty: Option<String>,
    err_ty: Option<String>,
}

struct Gen<'a> {
    resolve: &'a Resolve,
    queries: Vec<FieldSdl>,
    mutations: Vec<FieldSdl>,
    /// Named types reachable from output (return) positions — emitted as `type`/`enum`/…
    needed_types: BTreeSet<usize>,
    /// Named record/flags types reachable from argument positions — emitted as `input <Name>Input`.
    needed_input_types: BTreeSet<usize>,
    result_wrappers: Vec<ResultWrapper>,
}

impl<'a> Gen<'a> {
    fn new(resolve: &'a Resolve) -> Self {
        Self {
            resolve,
            queries: Vec::new(),
            mutations: Vec::new(),
            needed_types: BTreeSet::new(),
            needed_input_types: BTreeSet::new(),
            result_wrappers: Vec::new(),
        }
    }

    fn collect_world(&mut self, world: WorldId) {
        let world = &self.resolve.worlds[world];
        // Snapshot the items we need to walk so we can release the borrow on
        // `self.resolve.worlds[…]` before mutating self.
        let items: Vec<(WorldKey, WorldItem)> =
            world.exports.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (key, item) in items {
            match item {
                WorldItem::Function(func) => {
                    let name = match &key {
                        WorldKey::Name(n) => n.clone(),
                        WorldKey::Interface(_) => func.name.clone(),
                    };
                    self.collect_function(None, &name, &func);
                }
                WorldItem::Type { id, .. } => {
                    self.needed_types.insert(id.index());
                }
                WorldItem::Interface { id, .. } => {
                    // Prefer the world's export-key name (`export incidents-v2;` in WIT yields
                    // `WorldKey::Name("incidents-v2")`); fall back to the interface's own
                    // `name` field if the world exported it anonymously.
                    let iface_kebab = match &key {
                        WorldKey::Name(n) => Some(n.clone()),
                        WorldKey::Interface(_) => self.resolve.interfaces[id].name.clone(),
                    };
                    self.collect_interface(id, iface_kebab.as_deref());
                }
            }
        }
    }

    fn collect_interface(&mut self, id: wit_parser::InterfaceId, iface_kebab: Option<&str>) {
        let funcs: Vec<(String, Function)> = self.resolve.interfaces[id]
            .functions
            .iter()
            .map(|(n, f)| (n.clone(), f.clone()))
            .collect();
        for (name, func) in funcs {
            self.collect_function(iface_kebab, &name, &func);
        }
    }

    fn collect_function(
        &mut self,
        iface_kebab: Option<&str>,
        func_kebab: &str,
        func: &Function,
    ) {
        // Function-level (camel) and wrapper-type (pascal) names are qualified by the
        // owning interface to keep GraphQL `Query` / `Mutation` fields and `*Result`
        // types unique across interfaces.
        //
        // Classification (Query vs Mutation) still uses the BARE function name — the
        // verb prefix lives on the function (e.g. `create-`), not the interface.
        let qualified = match iface_kebab {
            Some(i) => format!("{}-{}", i, func_kebab),
            None => func_kebab.to_string(),
        };
        let camel = to_camel_case(&qualified);
        let pascal = to_pascal_case(&qualified);
        let kind = classify(func_kebab);

        // Arguments must use GraphQL `input` types, so format them in input position.
        let args: Vec<(String, String)> = func
            .params
            .iter()
            .map(|p| (to_camel_case(&p.name), self.format_type_input(p.ty)))
            .collect();

        let return_ty = match func.result {
            None => "Boolean!".to_string(),
            Some(ty) => self.format_return_type(&pascal, ty),
        };

        let field = FieldSdl {
            name: camel,
            args,
            return_ty,
        };
        match kind {
            FieldKind::Query => self.queries.push(field),
            FieldKind::Mutation => self.mutations.push(field),
        }
    }

    fn format_return_type(&mut self, pascal: &str, ty: Type) -> String {
        if let Type::Id(id) = ty {
            let def = &self.resolve.types[id];
            if let TypeDefKind::Result(Result_ { ok, err }) = &def.kind {
                return self.emit_result_wrapper(pascal, *ok, *err);
            }
        }
        self.format_type(ty)
    }

    fn emit_result_wrapper(
        &mut self,
        pascal: &str,
        ok: Option<Type>,
        err: Option<Type>,
    ) -> String {
        match (ok, err) {
            (None, None) => "Boolean!".to_string(),
            (Some(t), None) => self.format_type(t),
            (None, Some(_)) => "Boolean!".to_string(),
            (Some(ok_ty), Some(err_ty)) => {
                let ok_str = self.format_type(ok_ty);
                let err_str = self.format_type(err_ty);
                self.result_wrappers.push(ResultWrapper {
                    pascal: pascal.to_string(),
                    ok_ty: Some(ok_str),
                    err_ty: Some(err_str),
                });
                format!("{}Result!", pascal)
            }
        }
    }

    fn format_type(&mut self, ty: Type) -> String {
        match ty {
            Type::Bool => "Boolean!".into(),
            Type::U8 | Type::U16 | Type::U32 | Type::S8 | Type::S16 | Type::S32 => "Int!".into(),
            Type::U64 | Type::S64 => "String!".into(),
            Type::F32 | Type::F64 => "Float!".into(),
            Type::Char => "String!".into(),
            Type::String => "String!".into(),
            Type::ErrorContext => "String!".into(),
            Type::Id(id) => self.format_type_id(id),
        }
    }

    fn format_type_id(&mut self, id: TypeId) -> String {
        let def = &self.resolve.types[id];
        // If this TypeDef has a name, emit it as a top-level named GraphQL type.
        if def.name.is_some() {
            // Only treat as a top-level emission target if the kind is one we emit
            // (record, enum, variant, flags, resource).
            match def.kind {
                TypeDefKind::Record(_)
                | TypeDefKind::Enum(_)
                | TypeDefKind::Variant(_)
                | TypeDefKind::Flags(_)
                | TypeDefKind::Resource => {
                    self.needed_types.insert(id.index());
                    return format!("{}!", self.qualified_type_name(id));
                }
                _ => {
                    // Named alias/option/list/etc. — fall through and format kind directly.
                }
            }
        }
        self.format_type_kind(&def.kind)
    }

    /// Pascal-case GraphQL type name, prefixed by the owning interface name when the
    /// type lives inside an interface. Records named `list-op-params` in the
    /// `incidents-v2` interface get rendered as `IncidentsV2ListOpParams`; the same
    /// record name in `alerts-v2` becomes `AlertsV2ListOpParams`, avoiding collisions.
    fn qualified_type_name(&self, id: TypeId) -> String {
        let def = &self.resolve.types[id];
        let name = def.name.as_deref().unwrap_or("");
        let prefix = match def.owner {
            TypeOwner::Interface(iid) => self.resolve.interfaces[iid].name.as_deref(),
            _ => None,
        };
        match prefix {
            Some(p) => to_pascal_case(&format!("{p}-{name}")),
            None => to_pascal_case(name),
        }
    }

    fn format_type_kind(&mut self, kind: &TypeDefKind) -> String {
        match kind {
            TypeDefKind::Option(inner) => {
                let mut s = self.format_type(*inner);
                if s.ends_with('!') {
                    s.pop();
                }
                s
            }
            TypeDefKind::List(inner) => {
                let inner = self.format_type(*inner);
                format!("[{}]!", inner)
            }
            TypeDefKind::Type(inner) => self.format_type(*inner),
            TypeDefKind::Result(Result_ { ok, err }) => {
                // Anonymous (non-top-level) result — degrade to ok payload or Boolean.
                match (ok, err) {
                    (Some(t), _) => self.format_type(*t),
                    (None, _) => "Boolean!".into(),
                }
            }
            TypeDefKind::Tuple(t) => {
                // Render as a List of the unified type if uniform, else String! placeholder.
                if let Some(first) = t.types.first() {
                    if t.types.iter().all(|x| x == first) {
                        let inner = self.format_type(*first);
                        return format!("[{}]!", inner);
                    }
                }
                "String!".into()
            }
            TypeDefKind::Map(_, _)
            | TypeDefKind::FixedLengthList(_, _)
            | TypeDefKind::Future(_)
            | TypeDefKind::Stream(_)
            | TypeDefKind::Handle(_)
            | TypeDefKind::Unknown => "String!".into(),
            // These are nominal — should have been caught by format_type_id when named.
            // Anonymous occurrences are unusual but emit a placeholder.
            TypeDefKind::Record(_)
            | TypeDefKind::Variant(_)
            | TypeDefKind::Enum(_)
            | TypeDefKind::Flags(_)
            | TypeDefKind::Resource => "String!".into(),
        }
    }

    /// The GraphQL `input` type name for a record/flags type: its output PascalCase name plus an
    /// `Input` suffix, so an input and output view of the same WIT type never collide.
    fn input_type_name(&self, id: TypeId) -> String {
        format!("{}Input", self.qualified_type_name(id))
    }

    /// Format a type used in **argument** position. Records/flags become `input` types; enums are
    /// shared with the output side; scalars/options/lists behave as in output position.
    fn format_type_input(&mut self, ty: Type) -> String {
        match ty {
            Type::Id(id) => self.format_type_id_input(id),
            other => self.format_type(other),
        }
    }

    fn format_type_id_input(&mut self, id: TypeId) -> String {
        let def = &self.resolve.types[id];
        if def.name.is_some() {
            match def.kind {
                TypeDefKind::Record(_) | TypeDefKind::Flags(_) => {
                    self.needed_input_types.insert(id.index());
                    return format!("{}!", self.input_type_name(id));
                }
                TypeDefKind::Enum(_) => {
                    // Enums are valid in input position; reuse the shared enum emission.
                    self.needed_types.insert(id.index());
                    return format!("{}!", self.qualified_type_name(id));
                }
                TypeDefKind::Variant(_) | TypeDefKind::Resource => {
                    // Not representable as a GraphQL input type — degrade to an opaque string.
                    return "String!".into();
                }
                _ => {
                    // Named alias/option/list — fall through and format the kind directly.
                }
            }
        }
        self.format_type_kind_input(&def.kind)
    }

    fn format_type_kind_input(&mut self, kind: &TypeDefKind) -> String {
        match kind {
            TypeDefKind::Option(inner) => {
                let mut s = self.format_type_input(*inner);
                if s.ends_with('!') {
                    s.pop();
                }
                s
            }
            TypeDefKind::List(inner) => {
                let inner = self.format_type_input(*inner);
                format!("[{}]!", inner)
            }
            TypeDefKind::Type(inner) => self.format_type_input(*inner),
            // Scalars, tuples, and anonymous fallbacks are identical to output position.
            other => self.format_type_kind(other),
        }
    }

    fn render(&mut self, source_label: &str) -> anyhow::Result<String> {
        // Resolve transitively-referenced named types (e.g. record fields referencing other records),
        // for both output and input (argument) positions.
        self.close_types();

        let mut out = String::new();
        writeln!(out, "# Generated from {} by wit-to-gql", source_label)?;
        writeln!(out)?;

        // 1. Named types in topological order (resolve.types is already topologically sorted).
        let named: Vec<TypeId> = self
            .resolve
            .types
            .iter()
            .filter(|(id, _)| self.needed_types.contains(&id.index()))
            .map(|(id, _)| id)
            .collect();
        for id in named {
            self.emit_named_type(&mut out, id)?;
        }

        // 2. Result wrappers + unions.
        for w in &self.result_wrappers {
            let ok_field = w.ok_ty.clone().unwrap_or_else(|| "Boolean!".into());
            let err_field = w.err_ty.clone().unwrap_or_else(|| "String!".into());
            writeln!(out, "type {}Ok {{", w.pascal)?;
            writeln!(out, "  value: {}", ok_field)?;
            writeln!(out, "}}")?;
            writeln!(out)?;
            writeln!(out, "type {}Err {{", w.pascal)?;
            writeln!(out, "  error: {}", err_field)?;
            writeln!(out, "}}")?;
            writeln!(out)?;
            writeln!(
                out,
                "union {pascal}Result = {pascal}Ok | {pascal}Err",
                pascal = w.pascal
            )?;
            writeln!(out)?;
        }

        // 3. Input types (records/flags reached from argument positions). GraphQL requires
        // arguments to use `input` types, so these are emitted separately from output `type`s.
        let input_named: Vec<TypeId> = self
            .resolve
            .types
            .iter()
            .filter(|(id, _)| self.needed_input_types.contains(&id.index()))
            .map(|(id, _)| id)
            .collect();
        for id in input_named {
            self.emit_input_type(&mut out, id)?;
        }

        // 4. Root types.
        if !self.queries.is_empty() {
            writeln!(out, "type Query {{")?;
            for f in &self.queries {
                write_field(&mut out, f)?;
            }
            writeln!(out, "}}")?;
            writeln!(out)?;
        } else {
            // GraphQL requires a Query root. Emit a placeholder.
            writeln!(out, "type Query {{")?;
            writeln!(out, "  _empty: Boolean")?;
            writeln!(out, "}}")?;
            writeln!(out)?;
        }
        if !self.mutations.is_empty() {
            writeln!(out, "type Mutation {{")?;
            for f in &self.mutations {
                write_field(&mut out, f)?;
            }
            writeln!(out, "}}")?;
            writeln!(out)?;
        }

        Ok(out)
    }

    fn close_types(&mut self) {
        // Repeatedly walk known named types and pull in their referenced named types until both the
        // output and input sets reach a fixed point.
        loop {
            let out_snapshot = self.needed_types.clone();
            let in_snapshot = self.needed_input_types.clone();

            // Output position: every reachable named structural type is an output type.
            for idx in &out_snapshot {
                let id = type_id_at(self.resolve, *idx);
                let mut acc = Vec::new();
                let mut seen = BTreeSet::new();
                self.reachable_named(&self.resolve.types[id].kind, &mut acc, &mut seen);
                for ref_id in acc {
                    self.needed_types.insert(ref_id.index());
                }
            }

            // Input position: reachable records/flags are input types; enums (valid in input) and
            // any other named types are emitted as shared output types.
            for idx in &in_snapshot {
                let id = type_id_at(self.resolve, *idx);
                let mut acc = Vec::new();
                let mut seen = BTreeSet::new();
                self.reachable_named(&self.resolve.types[id].kind, &mut acc, &mut seen);
                for ref_id in acc {
                    match self.resolve.types[ref_id].kind {
                        TypeDefKind::Record(_) | TypeDefKind::Flags(_) => {
                            self.needed_input_types.insert(ref_id.index());
                        }
                        _ => {
                            self.needed_types.insert(ref_id.index());
                        }
                    }
                }
            }

            if out_snapshot == self.needed_types && in_snapshot == self.needed_input_types {
                break;
            }
        }
    }

    /// Collect all *named structural* types (record/enum/variant/flags/resource) reachable from
    /// `kind`, traversing through anonymous wrappers (`option`, `list`, `tuple`, `result`, type
    /// aliases, …). Without this, e.g. an enum reached only via `option<enum>` inside a record would
    /// be referenced but never emitted. `seen` guards against reference cycles.
    fn reachable_named(
        &self,
        kind: &TypeDefKind,
        acc: &mut Vec<TypeId>,
        seen: &mut BTreeSet<usize>,
    ) {
        let mut refs = Vec::new();
        collect_type_refs(kind, &mut refs);
        for id in refs {
            if !seen.insert(id.index()) {
                continue;
            }
            let def = &self.resolve.types[id];
            let structural = matches!(
                def.kind,
                TypeDefKind::Record(_)
                    | TypeDefKind::Enum(_)
                    | TypeDefKind::Variant(_)
                    | TypeDefKind::Flags(_)
                    | TypeDefKind::Resource
            );
            if def.name.is_some() && structural {
                acc.push(id);
            } else {
                self.reachable_named(&def.kind, acc, seen);
            }
        }
    }

    fn emit_named_type(&mut self, out: &mut String, id: TypeId) -> anyhow::Result<()> {
        let def: TypeDef = self.resolve.types[id].clone();
        if def.name.is_none() {
            return Ok(());
        }
        let name = self.qualified_type_name(id);
        match &def.kind {
            TypeDefKind::Record(r) => {
                writeln!(out, "type {} {{", name)?;
                for f in &r.fields {
                    let ty = self.format_type(f.ty);
                    writeln!(out, "  {}: {}", to_camel_case(&f.name), ty)?;
                }
                writeln!(out, "}}")?;
                writeln!(out)?;
            }
            TypeDefKind::Enum(e) => {
                writeln!(out, "enum {} {{", name)?;
                for c in &e.cases {
                    writeln!(out, "  {}", screaming_snake(&c.name))?;
                }
                writeln!(out, "}}")?;
                writeln!(out)?;
            }
            TypeDefKind::Variant(v) => {
                // Emit one wrapper object per case, then a union.
                let mut members = Vec::new();
                for case in &v.cases {
                    let case_pascal = format!("{}{}", name, to_pascal_case(&case.name));
                    writeln!(out, "type {} {{", case_pascal)?;
                    match case.ty {
                        Some(ty) => {
                            let ty_str = self.format_type(ty);
                            writeln!(out, "  value: {}", ty_str)?;
                        }
                        None => {
                            writeln!(out, "  _tag: Boolean")?;
                        }
                    }
                    writeln!(out, "}}")?;
                    writeln!(out)?;
                    members.push(case_pascal);
                }
                writeln!(out, "union {} = {}", name, members.join(" | "))?;
                writeln!(out)?;
            }
            TypeDefKind::Flags(f) => {
                writeln!(out, "type {} {{", name)?;
                for flag in &f.flags {
                    writeln!(out, "  {}: Boolean!", to_camel_case(&flag.name))?;
                }
                writeln!(out, "}}")?;
                writeln!(out)?;
            }
            TypeDefKind::Resource => {
                writeln!(out, "# resource {} (opaque handle)", name)?;
                writeln!(out, "scalar {}", name)?;
                writeln!(out)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Emit a record/flags type as a GraphQL `input` (used in argument position).
    fn emit_input_type(&mut self, out: &mut String, id: TypeId) -> anyhow::Result<()> {
        let def: TypeDef = self.resolve.types[id].clone();
        if def.name.is_none() {
            return Ok(());
        }
        let name = self.input_type_name(id);
        match &def.kind {
            TypeDefKind::Record(r) => {
                writeln!(out, "input {} {{", name)?;
                for f in &r.fields {
                    let ty = self.format_type_input(f.ty);
                    writeln!(out, "  {}: {}", to_camel_case(&f.name), ty)?;
                }
                writeln!(out, "}}")?;
                writeln!(out)?;
            }
            TypeDefKind::Flags(flags) => {
                writeln!(out, "input {} {{", name)?;
                for flag in &flags.flags {
                    writeln!(out, "  {}: Boolean!", to_camel_case(&flag.name))?;
                }
                writeln!(out, "}}")?;
                writeln!(out)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn write_field(out: &mut String, f: &FieldSdl) -> anyhow::Result<()> {
    if f.args.is_empty() {
        writeln!(out, "  {}: {}", f.name, f.return_ty)?;
    } else {
        write!(out, "  {}(", f.name)?;
        for (i, (n, t)) in f.args.iter().enumerate() {
            if i > 0 {
                write!(out, ", ")?;
            }
            write!(out, "{}: {}", n, t)?;
        }
        writeln!(out, "): {}", f.return_ty)?;
    }
    Ok(())
}

fn collect_type_refs(kind: &TypeDefKind, refs: &mut Vec<TypeId>) {
    let mut push = |ty: Type| {
        if let Type::Id(id) = ty {
            refs.push(id);
        }
    };
    match kind {
        TypeDefKind::Record(r) => r.fields.iter().for_each(|f| push(f.ty)),
        TypeDefKind::Variant(v) => v.cases.iter().for_each(|c| {
            if let Some(t) = c.ty {
                push(t);
            }
        }),
        TypeDefKind::Tuple(t) => t.types.iter().copied().for_each(push),
        TypeDefKind::List(t)
        | TypeDefKind::Option(t)
        | TypeDefKind::Type(t)
        | TypeDefKind::FixedLengthList(t, _) => push(*t),
        TypeDefKind::Result(r) => {
            if let Some(t) = r.ok {
                push(t);
            }
            if let Some(t) = r.err {
                push(t);
            }
        }
        TypeDefKind::Map(k, v) => {
            push(*k);
            push(*v);
        }
        TypeDefKind::Future(t) | TypeDefKind::Stream(t) => {
            if let Some(t) = t {
                push(*t);
            }
        }
        TypeDefKind::Handle(h) => match h {
            wit_parser::Handle::Own(id) | wit_parser::Handle::Borrow(id) => refs.push(*id),
        },
        _ => {}
    }
}

fn type_id_at(resolve: &Resolve, index: usize) -> TypeId {
    resolve
        .types
        .iter()
        .find(|(id, _)| id.index() == index)
        .map(|(id, _)| id)
        .expect("type index in needed set must exist in resolve")
}

fn screaming_snake(kebab: &str) -> String {
    kebab.replace('-', "_").to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wit_parser::{Function, FunctionKind, Param, Result_, Stability, Type, TypeDefKind, World};

    fn empty_resolve_with_world() -> (Resolve, WorldId) {
        let mut resolve = Resolve::default();
        let world = resolve.worlds.alloc(World {
            name: "test".into(),
            imports: Default::default(),
            exports: Default::default(),
            package: None,
            docs: Default::default(),
            stability: Stability::Unknown,
            includes: vec![],
            span: Default::default(),
        });
        (resolve, world)
    }

    fn add_result_string_string(resolve: &mut Resolve) -> Type {
        let id = resolve.types.alloc(wit_parser::TypeDef {
            name: None,
            kind: TypeDefKind::Result(Result_ {
                ok: Some(Type::String),
                err: Some(Type::String),
            }),
            owner: wit_parser::TypeOwner::None,
            docs: Default::default(),
            stability: Stability::Unknown,
            span: Default::default(),
        });
        Type::Id(id)
    }

    #[test]
    fn classify_prefixes() {
        assert_eq!(classify("get-user"), FieldKind::Query);
        assert_eq!(classify("list-issues"), FieldKind::Query);
        assert_eq!(classify("search-code"), FieldKind::Query);
        assert_eq!(classify("create-repository"), FieldKind::Mutation);
        assert_eq!(classify("delete-file"), FieldKind::Mutation);
        assert_eq!(classify("fork-repository"), FieldKind::Mutation);
        assert_eq!(classify("issue-write"), FieldKind::Mutation);
        assert_eq!(classify("issue-read"), FieldKind::Query);
    }

    #[test]
    fn simple_get_user_function() {
        let (mut resolve, world_id) = empty_resolve_with_world();
        let result_ty = add_result_string_string(&mut resolve);
        let func = Function {
            name: "get-user".into(),
            kind: FunctionKind::Freestanding,
            params: vec![Param {
                name: "user-name".into(),
                ty: Type::String,
                span: Default::default(),
            }],
            result: Some(result_ty),
            docs: Default::default(),
            stability: Stability::Unknown,
            span: Default::default(),
        };
        resolve.worlds[world_id]
            .exports
            .insert(WorldKey::Name("get-user".into()), WorldItem::Function(func));

        let sdl = generate(&resolve, world_id, "test.wasm").unwrap();
        assert!(sdl.contains("type GetUserOk {"), "missing Ok wrapper:\n{}", sdl);
        assert!(sdl.contains("value: String!"));
        assert!(sdl.contains("type GetUserErr {"));
        assert!(sdl.contains("error: String!"));
        assert!(sdl.contains("union GetUserResult = GetUserOk | GetUserErr"));
        assert!(sdl.contains("type Query {"));
        assert!(sdl.contains("getUser(userName: String!): GetUserResult!"));
    }

    #[test]
    fn mutation_with_option() {
        let (mut resolve, world_id) = empty_resolve_with_world();
        let opt_string = resolve.types.alloc(wit_parser::TypeDef {
            name: None,
            kind: TypeDefKind::Option(Type::String),
            owner: wit_parser::TypeOwner::None,
            docs: Default::default(),
            stability: Stability::Unknown,
            span: Default::default(),
        });
        let result_ty = add_result_string_string(&mut resolve);
        let func = Function {
            name: "create-repository".into(),
            kind: FunctionKind::Freestanding,
            params: vec![
                Param {
                    name: "name".into(),
                    ty: Type::String,
                    span: Default::default(),
                },
                Param {
                    name: "description".into(),
                    ty: Type::Id(opt_string),
                    span: Default::default(),
                },
                Param {
                    name: "private".into(),
                    ty: Type::Bool,
                    span: Default::default(),
                },
            ],
            result: Some(result_ty),
            docs: Default::default(),
            stability: Stability::Unknown,
            span: Default::default(),
        };
        resolve.worlds[world_id].exports.insert(
            WorldKey::Name("create-repository".into()),
            WorldItem::Function(func),
        );
        let sdl = generate(&resolve, world_id, "test.wasm").unwrap();
        assert!(sdl.contains("type Mutation {"), "expected Mutation block:\n{}", sdl);
        assert!(sdl.contains(
            "createRepository(name: String!, description: String, private: Boolean!): CreateRepositoryResult!"
        ), "field signature wrong:\n{}", sdl);
    }

    #[test]
    fn record_param_emits_input_type() {
        let (mut resolve, world_id) = empty_resolve_with_world();
        // record show-params { id: string }
        let rec = resolve.types.alloc(wit_parser::TypeDef {
            name: Some("show-params".into()),
            kind: TypeDefKind::Record(wit_parser::Record {
                fields: vec![wit_parser::Field {
                    name: "id".into(),
                    ty: Type::String,
                    docs: Default::default(),
                    span: Default::default(),
                }],
            }),
            owner: wit_parser::TypeOwner::None,
            docs: Default::default(),
            stability: Stability::Unknown,
            span: Default::default(),
        });
        let result_ty = add_result_string_string(&mut resolve);
        let func = Function {
            name: "show".into(),
            kind: FunctionKind::Freestanding,
            params: vec![Param {
                name: "params".into(),
                ty: Type::Id(rec),
                span: Default::default(),
            }],
            result: Some(result_ty),
            docs: Default::default(),
            stability: Stability::Unknown,
            span: Default::default(),
        };
        resolve.worlds[world_id]
            .exports
            .insert(WorldKey::Name("show".into()), WorldItem::Function(func));

        let sdl = generate(&resolve, world_id, "test.wasm").unwrap();
        // The argument record is emitted as a GraphQL `input`, referenced from the field argument,
        // and must NOT leak as an output `type` (which would be invalid in argument position).
        assert!(sdl.contains("input ShowParamsInput {"), "missing input type:\n{sdl}");
        assert!(sdl.contains("id: String!"), "{sdl}");
        assert!(
            sdl.contains("show(params: ShowParamsInput!): ShowResult!"),
            "field signature wrong:\n{sdl}"
        );
        assert!(
            !sdl.contains("type ShowParams "),
            "record leaked as an output type:\n{sdl}"
        );
    }
}
