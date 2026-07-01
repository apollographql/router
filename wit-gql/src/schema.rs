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
    needed_types: BTreeSet<usize>,
    result_wrappers: Vec<ResultWrapper>,
}

impl<'a> Gen<'a> {
    fn new(resolve: &'a Resolve) -> Self {
        Self {
            resolve,
            queries: Vec::new(),
            mutations: Vec::new(),
            needed_types: BTreeSet::new(),
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

        let args: Vec<(String, String)> = func
            .params
            .iter()
            .map(|p| (to_camel_case(&p.name), self.format_type(p.ty)))
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

    fn render(&mut self, source_label: &str) -> anyhow::Result<String> {
        // Resolve transitively-referenced named types (e.g. record fields referencing other records).
        self.close_named_types();

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

        // 3. Root types.
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

    fn close_named_types(&mut self) {
        // Repeatedly walk known named types and pull in their referenced named types
        // until the set is fixed.
        loop {
            let snapshot = self.needed_types.clone();
            for idx in &snapshot {
                let id = type_id_at(self.resolve, *idx);
                let def = &self.resolve.types[id];
                let mut refs = Vec::new();
                collect_type_refs(&def.kind, &mut refs);
                for ref_id in refs {
                    if matches!(
                        self.resolve.types[ref_id].kind,
                        TypeDefKind::Record(_)
                            | TypeDefKind::Enum(_)
                            | TypeDefKind::Variant(_)
                            | TypeDefKind::Flags(_)
                            | TypeDefKind::Resource
                    ) && self.resolve.types[ref_id].name.is_some()
                    {
                        self.needed_types.insert(ref_id.index());
                    }
                }
            }
            if snapshot == self.needed_types {
                break;
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
}
