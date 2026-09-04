use std::borrow::Cow;
use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use indexmap::map::Entry;

use crate::bail;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::error::suggestion::did_you_mean;
use crate::error::suggestion::suggestion_list;
use crate::link::ElementName;
use crate::link::Import;
use crate::link::Link;
use crate::link::link_spec_definition::CORE_VERSIONS;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::link_spec_definition::LinkSpecDefinition;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec_registry::SPEC_REGISTRY;
use crate::schema::FederationSchema;
use crate::schema::position::TypeDefinitionPosition;

/// Metadata about a type/directive belonging to an @link application.
#[derive(Clone, Debug)]
pub struct LinkedElement {
    /// Metadata about the @link application the type/directive belongs to.
    pub link: Arc<Link>,
    /// The name-in-spec of the type/directive. Note that if the type/directive uses a default name
    /// but its name-in-spec was imported already under a different name (a.k.a. a shadowing
    /// import) or its name-in-spec is an invalid name, this method will still consider it to belong
    /// to that @link application, but its name-in-spec will be `None`.
    pub name_in_spec: Option<Name>,
}

/// Metadata about all the applications of @link in a schema.
// PORT_NOTE: Named `CoreFeatures` in the JS codebase, but "core" is outdated terminology. Note that
//            while the JS version allowed adding/removing @link applications, this version does not
//            and instead computes all metadata from the whole schema at construction. This is less
//            efficient, but slightly less bug-prone; if this becomes a hot function in the future,
//            we should consider splitting this up to similarly allow @link addition/removal. A
//            result of this change is that `conflictsByAlias` from the JS codebase doesn't need to
//            be tracked here.
#[derive(Clone, Debug)]
pub struct LinksMetadata {
    /// The [Link] for the link spec (or core spec) for the schema.
    link_itself: Arc<Link>,
    /// The [LinkSpecDefinition] for the link spec (or core spec) for the schema.
    link_spec_definition: &'static LinkSpecDefinition,
    /// For specs, a map from their names-in-schema to their [Link]s.
    // PORT_NOTE: Named `byAlias` in the JS codebase, but this was confusing terminology.
    by_spec_name_in_schema: IndexMap<Arc<str>, Arc<Link>>,
    /// For specs, a map from their identities to their [Link]s plus another map from imported
    /// type/directive names-in-spec to names-in-schema.
    by_identity: IndexMap<Identity, (Arc<Link>, IndexMap<ElementName, ElementName>)>,
    /// For imported types/directives, this is a map from their names-in-schema to their [Link]s
    /// plus names-in-spec.
    // PORT_NOTE: Named `byImportName` in the JS codebase, but this was confusing terminology.
    by_import_element_name_in_schema: IndexMap<ElementName, (Arc<Link>, ElementName)>,
}

impl LinksMetadata {
    /// Create @link metadata from a schema. Specifically it:
    /// 1. Finds the bootstrapping @link directive and definition (and ensures there's only one).
    /// 2. Runs link spec validations against @link applications.
    /// 3. Checks if imports are for known elements for @link applications with known specs.
    /// 4. Collects metadata about @link applications.
    ///
    /// It does not:
    /// 1. Expand definitions for any linked specs (including the link spec itself).
    /// 2. Require definitions to be expanded, other than for the bootstrapping @link directive.
    /// 3. Run non-link spec validations against @link applications with known specs.
    /// 4. Run validations to ensure no used shadowing imports (this must be done separately, after
    ///    definitions have been expanded and referencers collected).
    // PORT_NOTE: As noted in the comment on `LinksMetadata`, this method constructs metadata all at
    //            once instead of through individual @link adding removing. Unlike the JS codebase,
    //            this method doesn't expand definitions for known versions of the federation spec.
    pub fn from_schema(schema: &Schema) -> Result<Option<LinksMetadata>, FederationError> {
        // This finds "bootstrap" uses of @link/@core regardless of order. By spec, the bootstrap
        // directive application must be the first application of @link/@core, but this was not
        // enforced by the JS implementation, so we match it for backward compatibility.
        let mut bootstrap_directives = schema
            .schema_definition
            .directives
            .iter()
            .filter(|d| LinkSpecDefinition::is_bootstrap_directive(schema, d));
        let Some(bootstrap_directive) = bootstrap_directives.next() else {
            return Ok(None);
        };
        // There must be exactly one bootstrap directive.
        if let Some(extraneous_directive) = bootstrap_directives.next() {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    "Cannot link the link/core feature itself in `{}` since it has already been linked in `{}`",
                    extraneous_directive.serialize().no_indent(),
                    bootstrap_directive.serialize().no_indent()
                )
            }.into());
        }
        // Attempt to parse the bootstrap directive while not knowing the link spec version, but
        // only keep the URL (which tells us the link spec version).
        let url =
            Link::from_directive_application_when_link_spec_unknown(bootstrap_directive, schema)?
                .url;
        let link_spec_definition = if url.identity == Identity::link_identity() {
            LINK_VERSIONS.find(&url.version).ok_or_else(|| {
                SingleFederationError::UnknownLinkVersion {
                    message: format!(
                        r#"Schema uses unknown version {} of the {} spec"#,
                        url.version,
                        Identity::LINK_NAME,
                    ),
                }
            })
        } else if url.identity == Identity::core_identity() {
            CORE_VERSIONS.find(&url.version).ok_or_else(|| {
                SingleFederationError::UnknownLinkVersion {
                    message: format!(
                        r#"Schema uses unknown version {} of the {} spec"#,
                        url.version,
                        Identity::CORE_NAME,
                    ),
                }
            })
        } else {
            bail!("Unexpectedly found non-link/core URL for bootstrap directive");
        }?;

        // Now that we know the @link/@core directive's name-in-schema and the `LinkSpecDefinition`,
        // we can properly parse each @link/@core directive application.
        let link_directive_name_in_schema = &bootstrap_directive.name;
        let mut link_itself: Option<_> = None;
        let mut by_spec_name_in_schema = Default::default();
        let mut by_identity = Default::default();
        let mut by_import_element_name_in_schema = Default::default();
        // For composed elements, merge will generally keep the names-in-schema of spec elements in
        // subgraphs as a way to minimize conflicts while keeping element names predictable for
        // user-defined downstream code. However, merge will also sometimes change the spec of
        // certain spec elements (e.g. of a federation spec directive). The result of this is that
        // sometimes elements using a default name of one spec may be imported using another spec,
        // so we need to permit e.g. the cost spec to import "@cost" as "@federation__cost" in the
        // supergraph schema. This kind of thing is generally fine, provided the old spec's
        // name-in-schema is no longer in use in the supergraph schema.
        //
        // So whenever an import occurs with an element name-in-schema that uses a prefix but
        // there's no spec in the schema whose name-in-schema is that prefix, we store an entry here
        // from the yet-unused prefix to the element name-in-schema. This lets us easily lookup
        // those elements in `self.by_element_name_in_schema` if a later spec's name-in-schema ends
        // up equaling that prefix later and we need to generate an error message.
        //
        // PORT_NOTE: Unlike the JS code, we don't support @link addition/removal, so we only need
        // to remember the first such conflict (if we supported removals, we'd need to remember all
        // such conflicts).
        let mut first_conflict_by_spec_name_in_schema: IndexMap<Arc<str>, ElementName> =
            Default::default();
        for directive in schema.schema_definition.directives.iter() {
            // Ignore non-@link/@core directives.
            if &directive.name != link_directive_name_in_schema {
                continue;
            }
            // Properly parse the @link/@core directive application into a `Link`.
            let link = Arc::new(link_spec_definition.link_from_directive(directive, schema)?);
            // If this is for the bootstrapped link/core spec, record it.
            if link.url == url && link_itself.replace(link.clone()).is_some() {
                bail!("Unexpectedly multiple @link applications for the link/core feature");
            }
            Self::add_link(
                link,
                &mut by_spec_name_in_schema,
                &mut by_identity,
                &mut by_import_element_name_in_schema,
                &mut first_conflict_by_spec_name_in_schema,
            )?;
        }
        let Some(link_itself) = link_itself else {
            bail!("Unexpectedly no @link applications for the link/core feature");
        };

        Ok(Some(LinksMetadata {
            link_itself,
            link_spec_definition,
            by_spec_name_in_schema,
            by_identity,
            by_import_element_name_in_schema,
        }))
    }

    fn add_link(
        link: Arc<Link>,
        by_spec_name_in_schema: &mut IndexMap<Arc<str>, Arc<Link>>,
        by_identity: &mut IndexMap<Identity, (Arc<Link>, IndexMap<ElementName, ElementName>)>,
        by_import_element_name_in_schema: &mut IndexMap<ElementName, (Arc<Link>, ElementName)>,
        first_conflict_by_spec_name_in_schema: &mut IndexMap<Arc<str>, ElementName>,
    ) -> Result<(), FederationError> {
        let identity = &link.url.identity;
        // The identity can't already be mapped to another @link/`Link`. (Even when they're
        // different major versions, they're usually describing the same capabilities but in
        // incompatible ways, so we don't want to allow the same schema to try to use multiple of
        // them.)
        if by_identity.contains_key(identity) {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"Cannot link feature "{}" since it has already been linked in the schema."#,
                    identity,
                ),
            }
            .into());
        }

        let spec_name_in_schema = link.spec_name_in_schema();
        // Normally we'd always forbid "__" in spec names-in-schema. However, there are some older
        // supergraph schemas that link the "tag" and "inaccessible" specs to the names-in-schema
        // "federation__tag" and "federation__inaccessible". This is due to bugs in older versions
        // of composition, but is technically fine since these specs have no types and directives
        // other than the default directive, so they never prefix anything with "__". So we make a
        // very specific exception here for that case. We may remove this exception in the future,
        // once support has been dropped for those bugged composition versions.
        if !(identity == &Identity::tag_identity()
            && spec_name_in_schema == "federation__tag"
            && link.imports.is_empty()
            || identity == &Identity::inaccessible_identity()
                && spec_name_in_schema == "federation__inaccessible"
                && link.imports.is_empty())
        {
            // Don't allow spec names-in-schema to have "__" in them, as namespace splitting splits
            // on the earliest "__" (so a namespaced name with a spec name-in-schema containing "__"
            // would be erroneously split mid-spec-name-in-schema).
            if spec_name_in_schema.contains("__") {
                return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                    message: format!(
                        r#"Cannot link feature "{}" as "{}" since it contains "__". Please rename to a compliant name via "as"."#,
                        identity,
                        spec_name_in_schema,
                    ),
                }.into());
            }
        }
        // Don't allow spec names-in-schema to end in "_", as namespace splitting splits on the
        // earliest "__" (so a namespaced name with a spec name-in-schema ending with "_" would end
        // up with "___", and be split before the ending "_" instead of after).
        if spec_name_in_schema.ends_with("_") {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"Cannot link feature "{}" as "{}" since it ends in "_". Please rename to a compliant name via "as"."#,
                    identity,
                    spec_name_in_schema,
                ),
            }.into());
        }
        // Ideally here, we wouldn't allow spec names-in-schema to not be valid GraphQL names.
        // However, enough supergraph schemas use "." and "-" after the first character that we
        // can't impose that validation now. So instead, we match using a slightly relaxed regex
        // that allows "." and "-" after the first character. For schemas that have "." or "-", they
        // won't be able to use namespaced names for their spec schema elements due to GraphQL
        // validation, but imports will still work. This is why there's a `Name::new_unchecked()`
        // call in `Link::spec_name_in_schema()`.
        //
        // Note the error message below purposely says "not a valid GraphQL name" because we want to
        // encourage users to actually use GraphQL names and avoid creating more exceptional cases.
        if !SPEC_NAME_IN_SCHEMA_REGEX.is_match(&spec_name_in_schema) {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"Cannot link feature "{}" as "{}" since it is not a valid GraphQL name. Please rename to a compliant name via "as"."#,
                    identity,
                    spec_name_in_schema,
                ),
            }.into());
        }
        // Don't allow spec names-in-schema to conflict with previous imports that use "__" with a
        // prefix equal to that spec name-in-schema.
        if let Some(conflict_import_element_name_in_schema) =
            first_conflict_by_spec_name_in_schema.get(spec_name_in_schema.as_str())
        {
            let Some((conflict_link, conflict_import_element_name_in_spec)) =
                by_import_element_name_in_schema.get(conflict_import_element_name_in_schema)
            else {
                bail!("Unexpectedly cannot find link for import");
            };
            let conflict_identity = &conflict_link.url.identity;
            Self::check_tag_inaccessible_conflict(conflict_identity, identity)?;
            let import_error_message = if conflict_import_element_name_in_spec
                == conflict_import_element_name_in_schema
            {
                format!(r#""{}""#, conflict_import_element_name_in_spec)
            } else {
                format!(
                    r#""{}" as "{}""#,
                    conflict_import_element_name_in_spec, conflict_import_element_name_in_schema
                )
            };
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"Cannot import {} from feature "{}" since it can be confused with a namespaced name from another linked feature "{}". Please rename the import or feature to avoid conflicts via "as"."#,
                    import_error_message,
                    conflict_identity,
                    identity,
                ),
            }.into());
        }
        // Don't allow spec names-in-schema to have default directive names that conflict with
        // previous imports.
        let conflict_import_element_name_in_schema = ElementName {
            name: spec_name_in_schema.clone(),
            is_directive: true,
        };
        if let Some((conflict_link, conflict_import_element_name_in_spec)) =
            by_import_element_name_in_schema.get(&conflict_import_element_name_in_schema)
        {
            let conflict_identity = &conflict_link.url.identity;
            Self::check_tag_inaccessible_conflict(conflict_identity, identity)?;
            let import_error_message = if conflict_import_element_name_in_spec
                == &conflict_import_element_name_in_schema
            {
                format!(r#""{}""#, conflict_import_element_name_in_spec)
            } else {
                format!(
                    r#""{}" as "{}""#,
                    conflict_import_element_name_in_spec, conflict_import_element_name_in_schema
                )
            };
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"Cannot import {} from feature "{}" since it can be confused with a namespaced name from another linked feature "{}". Please rename the import or feature to avoid conflicts via "as"."#,
                    import_error_message,
                    conflict_identity,
                    identity
                ),
            }.into());
        }
        // The spec name-in-schema can't be already mapped to another @link/`Link`.
        if let Some(existing_link) = by_spec_name_in_schema.get(spec_name_in_schema.as_str()) {
            let existing_identity = &existing_link.url.identity;
            Self::check_tag_inaccessible_conflict(existing_identity, identity)?;
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"Cannot link feature {} as "{}" since another feature "{}" already uses that alias. Please rename the feature to avoid conflicts via "as"."#,
                    identity,
                    spec_name_in_schema,
                    existing_identity
                ),
            }.into());
        }

        let mut by_identity_imports: IndexMap<ElementName, ElementName> = Default::default();
        let known_element_names_in_spec: Option<IndexSet<ElementName>> = SPEC_REGISTRY
            .get_definition(&link.url)
            .map(|spec_definition| spec_definition.all_element_names().collect());
        for import in &link.imports {
            let import_element_name_in_spec = import.element_name_in_spec();
            let import_element_name_in_schema = import.element_name_in_schema();
            let import_error_message =
                if import_element_name_in_spec == import_element_name_in_schema {
                    format!(r#""{}""#, import_element_name_in_spec)
                } else {
                    format!(
                        r#""{}" as "{}""#,
                        import_element_name_in_spec, import_element_name_in_schema
                    )
                };

            // If the spec is known, the name-in-spec must be known.
            Self::validate_known_element_name_in_spec(
                &import_element_name_in_spec,
                &known_element_names_in_spec,
            )?;
            // Only allow mapping to a name with "__" if it's a no-op import or if the prefix isn't
            // equal to some existing spec's name-in-schema.
            if let Some((split_spec_name_in_schema, split_name_in_spec)) =
                Self::split_prefixed_name(&import_element_name_in_schema.name)
            {
                if split_spec_name_in_schema == spec_name_in_schema.as_str() {
                    if split_name_in_spec != import_element_name_in_spec.name.as_str() {
                        let split_element_name_in_spec = ElementName {
                            // Only used for error messages, so should be fine.
                            name: Name::new_unchecked(split_name_in_spec),
                            is_directive: import.is_directive,
                        };
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                r#"Cannot import {} from feature "{}" since it can be confused with the namespaced name for "{}". Please rename the import to avoid conflicts via "as"."#,
                                import_error_message,
                                identity,
                                split_element_name_in_spec
                            ),
                        }.into());
                    }
                } else {
                    if let Some(conflict_link) =
                        by_spec_name_in_schema.get(split_spec_name_in_schema)
                    {
                        let conflict_identity = &conflict_link.url.identity;
                        Self::check_tag_inaccessible_conflict(conflict_identity, identity)?;
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                r#"Cannot import {} from feature "{}" since it can be confused with a namespaced name from another linked feature "{}". Please rename the import or feature to avoid conflicts via "as"."#,
                                import_error_message,
                                identity,
                                conflict_identity
                            ),
                        }.into());
                    } else {
                        // As mentioned in the comment for `first_conflict_by_spec_name_in_schema`
                        // in the caller, we have to record the import in case a link gets added
                        // with the spec name-in-schema later.
                        first_conflict_by_spec_name_in_schema.insert(
                            split_spec_name_in_schema.into(),
                            import_element_name_in_schema.clone(),
                        );
                    }
                }
            }
            // For default directives, only allow mapping to a spec name-in-schema if it's a no-op
            // import.
            if import.is_directive {
                if import_element_name_in_schema.name == spec_name_in_schema {
                    if import_element_name_in_spec.name.as_str() != link.url.identity.name.as_ref()
                    {
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                r#"Cannot import {} from feature "{}" since it can be confused with the namespaced name for "@{}". Please rename the import to avoid conflicts via "as"."#,
                                import_error_message,
                                identity,
                                link.url.identity.name
                            ),
                        }.into());
                    }
                } else {
                    if let Some(conflict_link) =
                        by_spec_name_in_schema.get(import_element_name_in_schema.name.as_str())
                    {
                        let conflict_identity = &conflict_link.url.identity;
                        Self::check_tag_inaccessible_conflict(conflict_identity, identity)?;
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                r#"Cannot import {} from feature "{}" since it can be confused with a namespaced name from another linked feature "{}". Please rename the import or feature to avoid conflicts via "as"."#,
                                import_error_message,
                                identity,
                                conflict_identity
                            ),
                        }.into());
                    }
                }
            }
            // The name-in-spec can't be already mapped to a different name-in-schema.
            match by_identity_imports.entry(import_element_name_in_spec.clone()) {
                Entry::Occupied(entry) => {
                    let existing_element_name_in_schema = entry.get();
                    if existing_element_name_in_schema != &import_element_name_in_schema {
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                r#"Cannot import {} from feature "{}" since it was previously imported as "{}". Please remove one of these imports."#,
                                import_error_message,
                                identity,
                                existing_element_name_in_schema
                            ),
                        }.into());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(import_element_name_in_schema.clone());
                }
            }
            // The name-in-schema can't already be mapped to a different name-in-spec.
            match by_import_element_name_in_schema.entry(import_element_name_in_schema) {
                Entry::Occupied(entry) => {
                    let (existing_link, existing_element_name_in_spec) = entry.get();
                    let existing_identity = &existing_link.url.identity;
                    if existing_identity != identity {
                        Self::check_tag_inaccessible_conflict(existing_identity, identity)?;
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                r#"Cannot import {} from feature "{}" since it was previously imported from feature "{}". Please rename the import to avoid conflicts via "as"."#,
                                import_error_message,
                                identity,
                                existing_identity
                            ),
                        }.into());
                    }
                    if existing_element_name_in_spec != &import_element_name_in_spec {
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                r#"Cannot import {} from feature "{}" since it was previously imported for "{}". Please rename the import to avoid conflicts via "as"."#,
                                import_error_message,
                                identity,
                                existing_element_name_in_spec
                            ),
                        }.into());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert((link.clone(), import_element_name_in_spec));
                }
            }
        }
        by_spec_name_in_schema.insert(spec_name_in_schema.into(), link.clone());
        by_identity.insert(identity.clone(), (link, by_identity_imports));
        Ok(())
    }

    /// Returns whether a spec's name-in-schema would pass the checks in `add_link()`, except import
    /// conflicts are taken from the given map, which should be computed via
    /// `[Self::compute_spec_name_in_schema_conflicts].
    pub(crate) fn is_spec_name_in_schema_valid(
        &self,
        spec_name_in_schema: &str,
        identity: &Identity,
        import_conflicts_by_identity: &ImportConflictsByIdentity,
    ) -> bool {
        // Don't allow spec names-in-schema to have "__" in them. Note that this method is only used
        // in merging to detect whether we need to rename the spec, so we don't need the exception
        // for "federation__tag" and "federation__inaccessible" here.
        if spec_name_in_schema.contains("__") {
            return false;
        }
        // Don't allow spec names-in-schema to end in "_".
        if spec_name_in_schema.ends_with("_") {
            return false;
        }
        // Don't allow spec names-in-schema to not be valid GraphQL names. Note that unlike
        // `add_link()`, we consider "." and "-" to not be valid here, but since this method is only
        // used in merging to detect whether we need to rename the spec, this has the effect of
        // ensuring that supergraph schemas don't use "." and "-" in their spec names-in-schema
        // (which will help later if we want to fully forbid "." and "-" in spec names-in-schema).
        if !GRAPHQL_NAME_REGEX.is_match(spec_name_in_schema) {
            return false;
        }
        for (conflict_identity, import_conflicts) in import_conflicts_by_identity {
            if identity == conflict_identity {
                // For import names namespaced using this spec name-in-schema, only allow them if
                // they're no-op imports.
                if import_conflicts.self_.contains(spec_name_in_schema) {
                    return false;
                }
            } else {
                // Don't allow imports of other specs that are namespaced by this spec
                // name-in-schema.
                if import_conflicts.other.contains(spec_name_in_schema) {
                    return false;
                }
            }
        }
        // The spec name-in-schema can't be already mapped to another @link/`Link`.
        if self
            .by_spec_name_in_schema
            .contains_key(spec_name_in_schema)
        {
            return false;
        }
        true
    }

    /// This is a method that helps us handle the case where:
    /// 1. directives of some spec are being composed into the supergraph (due to them being Apollo
    ///    specs or via `@composeDirective`),
    /// 2. those directives don't actually have any conflicts, and
    /// 3. the spec itself has spec name-in-schema conflicts when linked with the spec's name.
    ///
    /// For the `@composeDirective` case at least, you might think we could just use the
    /// `@link(as:)` rename from the subgraph, but when we established `@composeDirective` we never
    /// mandated that the spec names-in-schema be the same (just their composed directive
    /// names-in-schema). The reason we focused on directives was that downstream consumers were
    /// expecting certain directive names, so it makes sense to force agreement on a single name per
    /// directive across subgraphs. As part of that, we explicitly generate imports for all those
    /// directives, so the spec name-in-schema is never used for namespaced names via "__", and
    /// consumers consequently don't deal or care about those spec names-in-schema much. We could
    /// make a breaking change to force alignment on a spec name-in-schema to use in the supergraph,
    /// but spec name-in-schema agreement likely isn't valuable enough for a breakage. More
    /// importantly, it also doesn't really solve the case for conflicts linking Apollo specs.
    ///
    /// So instead, when we detect a conflict, we generate a unique spec name-in-schema. This method
    /// does two things:
    /// 1. Outputs data that can be used to efficiently detect import conflicts.
    /// 2. Outputs a closure that can be used to generate unique spec names-in-schema.
    ///
    /// For unique spec name-in-schema computation, we compute a non-conflicting prefix by using a
    /// trie to determine a GraphQL name that isn't a prefix of any existing names (this prefix also
    /// doesn't use "_" outside the first character, so it should be safe for spec names-in-schema).
    /// We then add an incrementing index to it, to account for @links that have the same spec name.
    /// Finally, we add the spec name but without any non-letter characters.
    pub(crate) fn compute_spec_name_in_schema_conflicts<'a>(
        spec_conflict_infos: impl IntoIterator<Item = SpecConflictInfo<'a>>,
        all_element_names: impl IntoIterator<Item = Name>,
    ) -> (ImportConflictsByIdentity, UniqueSpecNameInSchema) {
        // Generate `import_conflicts_by_identity` and track names for the trie.
        let mut trie_names: IndexSet<Arc<str>> = all_element_names
            .into_iter()
            .map(|n| n.clone().into())
            .collect();
        let mut import_conflicts_by_identity: ImportConflictsByIdentity = Default::default();
        for SpecConflictInfo {
            spec_name_in_schema,
            url,
            imports,
        } in spec_conflict_infos
        {
            trie_names.insert(spec_name_in_schema.clone());
            let mut self_: IndexSet<_> = Default::default();
            let mut other: IndexSet<_> = Default::default();
            for import in imports {
                let import_name_in_spec = &import.element;
                let import_name_in_schema = import.name_in_schema();
                trie_names.insert(import_name_in_schema.clone().into());
                if let Some((split_spec_name_in_schema, split_name_in_spec)) =
                    Self::split_prefixed_name(import_name_in_schema)
                {
                    if split_name_in_spec != import_name_in_spec.as_str() {
                        // Spec name-in-schema being `split_spec_name_in_schema` would generate a
                        // conflict due to the import not being a no-op import for a namespaced
                        // name.
                        self_.insert(split_spec_name_in_schema.into());
                    }
                    // Spec name-in-schema being `split_spec_name_in_schema` would generate a
                    // conflict due to this import from some other identity being namespaced to it.
                    other.insert(split_spec_name_in_schema.into());
                }
                if import.is_directive {
                    if import_name_in_spec.as_str() != url.identity.name.as_ref() {
                        // Spec name-in-schema being 'import_name_in_schema' would generate a
                        // conflict due to the import not being a no-op import for the default
                        // directive.
                        self_.insert(import_name_in_schema.clone().into());
                    }
                    // Spec name-in-schema being `import_name_in_schema` would generate a conflict
                    // due to this import from some other identity being the default directive for
                    // it.
                    other.insert(import_name_in_schema.clone().into());
                }
            }
            import_conflicts_by_identity
                .insert(url.identity.clone(), ImportConflict { self_, other });
        }
        // Create the closure for computing unique spec names-in-schema via a trie.
        let mut prefix: Option<String> = None;
        let mut index: usize = 0;
        let compute_unique_spec_name_in_schema = move |spec_name: &str| {
            let prefix = prefix.get_or_insert_with(|| {
                let mut root: TrieNode = Default::default();
                // Populate the trie.
                for name in &trie_names {
                    let mut node = &mut root;
                    for char in name.chars() {
                        node = node.children.entry(char).or_default();
                    }
                }
                // This arena keeps track of nodes we've already traversed in the BFS, in addition
                // to nodes we've queued but yet to traverse (starting at `head`). It's bounded by
                // the number of nodes in the trie, and each traversal entry is constant size.
                let mut nodes = vec![TrieTraversal {
                    node: &root,
                    // A sentinel value to indicate the root.
                    parent: usize::MAX,
                    char_: '\0',
                }];
                let mut head: usize = 0;
                loop {
                    let possible_chars = if head == 0 {
                        TRIE_SPEC_NAME_IN_SCHEMA_START
                    } else {
                        TRIE_SPEC_NAME_IN_SCHEMA_CONTINUE
                    };
                    let node = nodes[head].clone();
                    for char in possible_chars.chars() {
                        if let Some(child) = node.node.children.get(&char) {
                            nodes.push(TrieTraversal {
                                node: child,
                                parent: head,
                                char_: char,
                            })
                        } else {
                            let mut chars = vec![char];
                            let mut cur = &node;
                            while cur.parent != usize::MAX {
                                chars.push(cur.char_);
                                cur = &nodes[cur.parent];
                            }
                            return chars.into_iter().rev().collect();
                        }
                    }
                    head += 1;
                }
            });
            let suffix: String = spec_name
                .chars()
                .filter(|c| c.is_ascii_alphabetic())
                .collect();
            let unique_spec_name_in_schema = format!("{prefix}{index}{suffix}");
            index += 1;
            Name::new_unchecked(&unique_spec_name_in_schema)
        };
        (
            import_conflicts_by_identity,
            Box::new(compute_unique_spec_name_in_schema),
        )
    }

    /// There's a particular pattern in Fed 1 subgraphs, where they would try to link the "tag" or
    /// "inaccessible" specs directly instead of importing the directives from the "federation"
    /// spec, and this can cause a conflict. This function gives a more helpful error message in
    /// that case.
    fn check_tag_inaccessible_conflict(
        identity1: &Identity,
        identity2: &Identity,
    ) -> Result<(), FederationError> {
        let identities: IndexSet<_> = [identity1, identity2].into_iter().collect();
        if !identities.contains(&Identity::federation_identity()) {
            return Ok(());
        }
        let (directive, identity) = if identities.contains(&Identity::tag_identity()) {
            (Identity::TAG_NAME, Identity::tag_identity())
        } else if identities.contains(&Identity::inaccessible_identity()) {
            (
                Identity::INACCESSIBLE_NAME,
                Identity::inaccessible_identity(),
            )
        } else {
            return Ok(());
        };
        Err(SingleFederationError::InvalidLinkDirectiveUsage {
            message: format!(
                r#"Please import "@{}" from the feature "{}" instead of using "{}" to avoid potential unexpected behavior in the future."#,
                directive,
                Identity::federation_identity(),
                identity,
            ),
        }.into())
    }

    fn validate_known_element_name_in_spec(
        element_name_in_spec: &ElementName,
        known_element_names_in_spec: &Option<IndexSet<ElementName>>,
    ) -> Result<(), FederationError> {
        let Some(known_element_names_in_spec) = known_element_names_in_spec else {
            return Ok(());
        };
        if known_element_names_in_spec.contains(element_name_in_spec) {
            return Ok(());
        };
        let mut message_parts = vec![format!(
            r#"Cannot import unknown element "{}"."#,
            element_name_in_spec
        )];
        if !element_name_in_spec.is_directive
            && let known_element_name_in_spec = (ElementName {
                name: element_name_in_spec.name.clone(),
                is_directive: true,
            })
            && known_element_names_in_spec.contains(&known_element_name_in_spec)
        {
            message_parts.push(format!(
                r#" Did you mean directive "{}"?"#,
                known_element_name_in_spec
            ));
        } else if let suggestions = suggestion_list(
            &element_name_in_spec.to_string(),
            known_element_names_in_spec.iter().map(ToString::to_string),
        ) && !suggestions.is_empty()
        {
            // Note that the message returned by `did_you_mean()` has a leading space.
            message_parts.push(did_you_mean(suggestions))
        }
        Err(SingleFederationError::InvalidLinkDirectiveUsage {
            message: message_parts.join(""),
        }
        .into())
    }

    /// Assuming the [LinksMetadata] is for the given [FederationSchema], returns an error for each
    /// used schema element with a shadowing import. A "shadowing import" occurs when an element
    /// would normally belong to an @link application due to having a default name for its spec, but
    /// the name-in-spec has been imported already under a different name. Note that for
    /// backwards-compatibility reasons, we ignore shadowed types if they're only used by other
    /// shadowed elements.
    ///
    /// We enforce this validation because downstream code almost always assumes there's exactly one
    /// name for a spec element, and allowing multiple elements with the same @link application and
    /// name-in-spec will thus result in some of those elements being erroneously ignored. (This is
    /// similar to the validation that forbids importing the same name-in-spec with different
    /// names-in-schema, but that can't be easily done when recomputing [LinksMetadata] due to a
    /// change in @link applications since it depends on what elements are actually in the schema,
    /// and that doesn't get finalized until later in the schema-building process.)
    pub(crate) fn validate_no_shadowing_imports(
        &self,
        schema: &FederationSchema,
    ) -> Result<(), FederationError> {
        let mut used_shadowing_imports: Vec<(ShadowingImport, String)> = Vec::new();
        for type_pos in schema.get_types() {
            let element_name_in_schema = ElementName {
                name: type_pos.type_name().clone(),
                is_directive: false,
            };
            let Some(shadowing_import) = self.get_shadowing_import(&element_name_in_schema) else {
                continue;
            };
            if self.has_non_shadowed_referencing_root_elements(schema, &type_pos) {
                used_shadowing_imports.push((shadowing_import, type_pos.to_string()));
            }
        }
        for directive_pos in schema.get_directive_definitions() {
            let element_name_in_schema = ElementName {
                name: directive_pos.directive_name.clone(),
                is_directive: true,
            };
            let Some(shadowing_import) = self.get_shadowing_import(&element_name_in_schema) else {
                continue;
            };
            if !schema
                .referencers()
                .get_directive(&directive_pos.directive_name)
                .is_empty()
            {
                used_shadowing_imports.push((shadowing_import, directive_pos.to_string()));
            };
        }
        if used_shadowing_imports.is_empty() {
            return Ok(());
        }
        Err(FederationError::MultipleFederationErrors(MultipleFederationErrors {
            errors: used_shadowing_imports.iter().map(|(shadowing_import, coordinate)| {
                let import_error_message = if shadowing_import.import_element_name_in_spec ==
                    shadowing_import.import_element_name_in_schema
                {
                    format!(r#""{}""#, shadowing_import.import_element_name_in_spec)
                } else {
                    format!(
                        r#""{}" as "{}""#,
                        shadowing_import.import_element_name_in_spec,
                        shadowing_import.import_element_name_in_schema
                    )
                };
                SingleFederationError::InvalidLinkDirectiveUsage {
                    message: format!(
                        r#"Cannot import {} from feature "{}" since there's a used definition for the namespaced name "{}". Please switch usages of the namespaced name to the import name and remove the definition."#,
                        import_error_message,
                        shadowing_import.link.url.identity,
                        coordinate,
                    )
                }
            }).collect(),
        }))
    }

    fn has_non_shadowed_referencing_root_elements(
        &self,
        schema: &FederationSchema,
        type_definition_position: &TypeDefinitionPosition,
    ) -> bool {
        match type_definition_position {
            TypeDefinitionPosition::Scalar(type_pos) => {
                let Some(referencers) = schema.referencers().scalar_types.get(&type_pos.type_name)
                else {
                    return false;
                };
                referencers
                    .object_fields
                    .iter()
                    .map(|field_pos| ElementName {
                        name: field_pos.type_name.clone(),
                        is_directive: false,
                    })
                    .chain(
                        referencers
                            .object_field_arguments
                            .iter()
                            .map(|arg_pos| ElementName {
                                name: arg_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .interface_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(referencers.interface_field_arguments.iter().map(|arg_pos| {
                        ElementName {
                            name: arg_pos.type_name.clone(),
                            is_directive: false,
                        }
                    }))
                    .chain(
                        referencers
                            .union_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .input_object_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .directive_arguments
                            .iter()
                            .map(|arg_pos| ElementName {
                                name: arg_pos.directive_name.clone(),
                                is_directive: true,
                            }),
                    )
                    .any(|element_name_in_schema| {
                        self.get_shadowing_import(&element_name_in_schema).is_none()
                    })
            }
            TypeDefinitionPosition::Object(type_pos) => {
                let Some(referencers) = schema.referencers().object_types.get(&type_pos.type_name)
                else {
                    return false;
                };
                if !referencers.schema_roots.is_empty() {
                    return true;
                }
                referencers
                    .object_fields
                    .iter()
                    .map(|field_pos| ElementName {
                        name: field_pos.type_name.clone(),
                        is_directive: false,
                    })
                    .chain(
                        referencers
                            .interface_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(referencers.union_types.iter().map(|type_pos| ElementName {
                        name: type_pos.type_name.clone(),
                        is_directive: false,
                    }))
                    .any(|element_name_in_schema| {
                        self.get_shadowing_import(&element_name_in_schema).is_none()
                    })
            }
            TypeDefinitionPosition::Interface(type_pos) => {
                let Some(referencers) = schema
                    .referencers()
                    .interface_types
                    .get(&type_pos.type_name)
                else {
                    return false;
                };
                referencers
                    .object_types
                    .iter()
                    .map(|type_pos| ElementName {
                        name: type_pos.type_name.clone(),
                        is_directive: false,
                    })
                    .chain(
                        referencers
                            .object_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .interface_types
                            .iter()
                            .map(|type_pos| ElementName {
                                name: type_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .interface_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .any(|element_name_in_schema| {
                        self.get_shadowing_import(&element_name_in_schema).is_none()
                    })
            }
            TypeDefinitionPosition::Union(type_pos) => {
                let Some(referencers) = schema.referencers().union_types.get(&type_pos.type_name)
                else {
                    return false;
                };
                referencers
                    .object_fields
                    .iter()
                    .map(|field_pos| ElementName {
                        name: field_pos.type_name.clone(),
                        is_directive: false,
                    })
                    .chain(
                        referencers
                            .interface_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .any(|element_name_in_schema| {
                        self.get_shadowing_import(&element_name_in_schema).is_none()
                    })
            }
            TypeDefinitionPosition::Enum(type_pos) => {
                let Some(referencers) = schema.referencers().enum_types.get(&type_pos.type_name)
                else {
                    return false;
                };
                referencers
                    .object_fields
                    .iter()
                    .map(|field_pos| ElementName {
                        name: field_pos.type_name.clone(),
                        is_directive: false,
                    })
                    .chain(
                        referencers
                            .object_field_arguments
                            .iter()
                            .map(|arg_pos| ElementName {
                                name: arg_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .interface_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(referencers.interface_field_arguments.iter().map(|arg_pos| {
                        ElementName {
                            name: arg_pos.type_name.clone(),
                            is_directive: false,
                        }
                    }))
                    .chain(
                        referencers
                            .input_object_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .directive_arguments
                            .iter()
                            .map(|arg_pos| ElementName {
                                name: arg_pos.directive_name.clone(),
                                is_directive: true,
                            }),
                    )
                    .any(|element_name_in_schema| {
                        self.get_shadowing_import(&element_name_in_schema).is_none()
                    })
            }
            TypeDefinitionPosition::InputObject(type_pos) => {
                let Some(referencers) = schema
                    .referencers()
                    .input_object_types
                    .get(&type_pos.type_name)
                else {
                    return false;
                };
                referencers
                    .object_field_arguments
                    .iter()
                    .map(|arg_pos| ElementName {
                        name: arg_pos.type_name.clone(),
                        is_directive: false,
                    })
                    .chain(referencers.interface_field_arguments.iter().map(|arg_pos| {
                        ElementName {
                            name: arg_pos.type_name.clone(),
                            is_directive: false,
                        }
                    }))
                    .chain(
                        referencers
                            .input_object_fields
                            .iter()
                            .map(|field_pos| ElementName {
                                name: field_pos.type_name.clone(),
                                is_directive: false,
                            }),
                    )
                    .chain(
                        referencers
                            .directive_arguments
                            .iter()
                            .map(|arg_pos| ElementName {
                                name: arg_pos.directive_name.clone(),
                                is_directive: true,
                            }),
                    )
                    .any(|element_name_in_schema| {
                        self.get_shadowing_import(&element_name_in_schema).is_none()
                    })
            }
        }
    }

    fn get_shadowing_import(
        &self,
        element_name_in_schema: &ElementName,
    ) -> Option<ShadowingImport<'_>> {
        let (link, element_name_in_spec) = self.source_default_name(element_name_in_schema)?;
        // An invalid name-in-spec can't be imported, and thus can't be shadowed.
        let element_name_in_spec = element_name_in_spec?;
        let import_element_name_in_schema =
            self.get_import_element_name_in_schema(link, &element_name_in_spec)?;
        // If the import is a no-op import (i.e. it imports an element to its default name), then
        // we don't consider it to shadow usages of the default name.
        if import_element_name_in_schema == element_name_in_schema {
            return None;
        }
        Some(ShadowingImport {
            link,
            import_element_name_in_spec: element_name_in_spec,
            import_element_name_in_schema: import_element_name_in_schema.clone(),
        })
    }

    // PORT_NOTE: Named `coreItself` in the JS codebase, but "core" is outdated terminology.
    pub(crate) fn link_itself(&self) -> &Arc<Link> {
        &self.link_itself
    }

    pub(crate) fn link_spec_definition(&self) -> &'static LinkSpecDefinition {
        self.link_spec_definition
    }

    pub(crate) fn by_import_element_name_in_schema(
        &self,
    ) -> &IndexMap<ElementName, (Arc<Link>, ElementName)> {
        &self.by_import_element_name_in_schema
    }

    // PORT_NOTE: Named `allFeatures` in the JS codebase, but "feature" is outdated terminology.
    pub fn all_links(&self) -> impl ExactSizeIterator<Item = &Arc<Link>> {
        self.by_identity.values().map(|(link, _)| link)
    }

    pub fn for_identity(&self, identity: &Identity) -> Option<Arc<Link>> {
        self.by_identity.get(identity).map(|(link, _)| link.clone())
    }

    pub fn source_link_of_type(&self, type_name: &Name) -> Option<LinkedElement> {
        let element_name_in_schema = ElementName {
            name: type_name.clone(),
            is_directive: false,
        };
        self.source_link(&element_name_in_schema)
    }

    pub fn source_link_of_directive(&self, directive_name: &Name) -> Option<LinkedElement> {
        let element_name_in_schema = ElementName {
            name: directive_name.clone(),
            is_directive: true,
        };
        self.source_link(&element_name_in_schema)
    }

    // PORT_NOTE: Named `sourceFeature` in the JS codebase, but "feature" is outdated terminology.
    fn source_link(&self, element_name_in_schema: &ElementName) -> Option<LinkedElement> {
        // Validations guarantee that names-in-schema don't collide with the default names of
        // different spec schema elements, so it doesn't technically matter which order we check
        // first. But we do have to some extra work for shadowing imports if we don't check imports
        // first, so we do that first.
        if let Some((link, import_element_name_in_spec)) = self
            .by_import_element_name_in_schema
            .get(element_name_in_schema)
        {
            return Some(LinkedElement {
                link: link.clone(),
                name_in_spec: Some(import_element_name_in_spec.name.clone()),
            });
        }
        // If it's not an import, check whether it's a default name for some existing spec.
        let (link, element_name_in_spec) = self.source_default_name(element_name_in_schema)?;
        Some(LinkedElement {
            link: link.clone(),
            name_in_spec: element_name_in_spec.and_then(|element_name_in_spec| {
                // Note that if an import's name-in-schema is the same as its default name, it's not
                // a shadowing import, and we should return a `Some` for `name_in_spec`. But if that
                // were true, we would have found an entry in `self.by_element_name_in_schema` above
                // when checking for imports. So we don't need to handle that case specially here.
                if self
                    .get_import_element_name_in_schema(link, &element_name_in_spec)
                    .is_none()
                {
                    Some(element_name_in_spec.name)
                } else {
                    None
                }
            }),
        })
    }

    /// Returns the name-in-schema of an imported type/directive for the given link and
    /// name-in-spec.
    // PORT_NOTE: Named `getImportName` in the JS codebase, but this was vague.
    fn get_import_element_name_in_schema(
        &self,
        link: &Arc<Link>,
        element_name_in_spec: &ElementName,
    ) -> Option<&ElementName> {
        self.by_identity
            .get(&link.url.identity)
            .and_then(|(_, by_name_in_spec)| by_name_in_spec.get(element_name_in_spec))
    }

    /// If the given element name is a default name (i.e. it's prefixed with an existing spec
    /// name-in-schema, or is a directive name for an existing spec name-in-schema), then return the
    /// [Link] for that spec along with the name-in-spec for the element (if valid).
    fn source_default_name(
        &self,
        element_name_in_schema: &ElementName,
    ) -> Option<(&Arc<Link>, Option<ElementName>)> {
        // Handle the prefixed case first.
        if let Some((spec_name_in_schema, name_in_spec)) =
            Self::split_prefixed_name(&element_name_in_schema.name)
        {
            // Note that we explicitly do not return `None` here if `link` isn't found, and instead
            // fall-through to the default directive name logic below. Normally that default
            // directive name logic would also return `None`, since validations above guarantee "__"
            // isn't in spec names-in-schema. But as noted above, we make an exception for the "tag"
            // and "inaccessible" specs for backwards-compatibility reasons, so we fall-through to
            // allow those exceptions to be found in `self.by_spec_name_in_schema`.
            if let Some(link) = self.by_spec_name_in_schema.get(spec_name_in_schema) {
                return Some((
                    link,
                    Name::new(name_in_spec).ok().map(|name| ElementName {
                        name,
                        is_directive: element_name_in_schema.is_directive,
                    }),
                ));
            }
        }
        // If not prefixed, then check whether it's the default directive name for a spec.
        if !element_name_in_schema.is_directive {
            return None;
        }
        let link = self
            .by_spec_name_in_schema
            .get(element_name_in_schema.name.as_str())?;
        let name_in_spec = link.url.identity.name.clone().try_into().ok();
        Some((
            link,
            name_in_spec.map(|name| ElementName {
                name,
                is_directive: true,
            }),
        ))
    }

    /// For type/directive names-in-schema that are prefixed to indicate they belong to a spec,
    /// return their spec name-in-schema and their type/directive name-in-spec (which may not be
    /// a valid GraphQL name).
    fn split_prefixed_name(name_in_schema: &Name) -> Option<(&str, &str)> {
        name_in_schema.split_once("__")
    }
}

pub(crate) type ImportConflictsByIdentity = IndexMap<Identity, ImportConflict>;

pub(crate) struct ImportConflict {
    /// Spec names-in-schema that would conflict with this identity.
    self_: IndexSet<Arc<str>>,
    /// Spec names-in-schema that would conflict with other identities than this one.
    other: IndexSet<Arc<str>>,
}

pub(crate) struct SpecConflictInfo<'a> {
    pub(crate) spec_name_in_schema: &'a Arc<str>,
    pub(crate) url: &'a Url,
    pub(crate) imports: Box<dyn Iterator<Item = Cow<'a, Import>> + 'a>,
}

pub(crate) type UniqueSpecNameInSchema = Box<dyn FnMut(&str) -> Name>;

/// Information about a shadowing import found for an element.
struct ShadowingImport<'a> {
    link: &'a Arc<Link>,
    import_element_name_in_spec: ElementName,
    import_element_name_in_schema: ElementName,
}

#[derive(Default)]
struct TrieNode {
    children: HashMap<char, TrieNode>,
}

#[derive(Clone)]
struct TrieTraversal<'a> {
    node: &'a TrieNode,
    parent: usize,
    char_: char,
}

static SPEC_NAME_IN_SCHEMA_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"^[_A-Za-z][_0-9A-Za-z.\-]*$"#).unwrap());

static GRAPHQL_NAME_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"^[_A-Za-z][_0-9A-Za-z]*$"#).unwrap());

static TRIE_SPEC_NAME_IN_SCHEMA_START: &str = concat!(
    "_",
    "abcdefghijklmnopqrstuvwxyz",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
);

static TRIE_SPEC_NAME_IN_SCHEMA_CONTINUE: &str = concat!(
    "abcdefghijklmnopqrstuvwxyz",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "0123456789"
);

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use apollo_compiler::name;

    use super::*;
    use crate::error::CompositionError;
    use crate::link::Import;
    use crate::link::Purpose;
    use crate::link::spec::Version;
    use crate::subgraph::typestate::Subgraph;

    fn errors(schema: &str) -> Vec<String> {
        // Note: we use `expand_links()` because currently it takes care of automatically adding
        // directive definitions, and we don't want to bother with adding the @link definition
        // to every example. `validate()` is chained on because expansion is a pure transformation —
        // shadowed-import errors are raised by the validation that follows it.
        let expanded =
            match Subgraph::parse("A", "", schema).and_then(|subgraph| subgraph.expand_links()) {
                Ok(expanded) => expanded,
                Err(error) => {
                    return error
                        .errors
                        .into_iter()
                        .map(|e| e.error.to_string())
                        .collect();
                }
            };
        let actual_errors: BTreeSet<_> = match expanded.validate() {
            Ok(_) => Default::default(),
            Err(failure) => failure
                .errors
                .into_iter()
                // Unwrapped rather than formatted, so the message matches the one the expansion
                // branch above produces (`CompositionError`'s `Display` prefixes the subgraph).
                .map(|error| match error {
                    CompositionError::SubgraphError { error, .. } => error.to_string(),
                    other => other.to_string(),
                })
                .collect(),
        };
        actual_errors.into_iter().collect()
    }

    #[test]
    fn explicit_root_directive_import() -> Result<(), FederationError> {
        let schema = r#"
          extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0", import: ["Import"])
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2", import: ["@inaccessible"])

          type Query { x: Int }

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          scalar Import

          directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "root_directive.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?;
        let meta = meta.expect("should have metadata");

        assert!(
            meta.source_link_of_directive(&name!("inaccessible"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn renamed_link_directive() -> Result<(), FederationError> {
        let schema = r#"
          extend schema
            @lonk(url: "https://specs.apollo.dev/link/v1.0", as: "lonk")
            @lonk(url: "https://specs.apollo.dev/inaccessible/v0.2")

          type Query { x: Int }

          enum lonk__Purpose {
            SECURITY
            EXECUTION
          }

          scalar lonk__Import

          directive @lonk(url: String!, as: String, import: [lonk__Import], for: lonk__Purpose) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "lonk.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?.expect("should have metadata");
        assert!(
            meta.source_link_of_directive(&name!("inaccessible"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn renamed_core_directive() -> Result<(), FederationError> {
        let schema = r#"
          extend schema
            @care(feature: "https://specs.apollo.dev/core/v0.2", as: "care")
            @care(feature: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)

          directive @care(feature: String!, as: String, for: core__Purpose) repeatable on SCHEMA
          directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

          type Query { x: Int }

          enum care__Purpose {
            SECURITY
            EXECUTION
          }

          scalar care__Import

          scalar join__FieldSet

          enum join__Graph {
            USERS @join__graph(name: "users", url: "http://localhost:4001")
          }
        "#;

        let schema = Schema::parse(schema, "care.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?.expect("should have metadata");
        assert!(
            meta.source_link_of_directive(&name!("join__graph"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn url_syntax() -> Result<(), FederationError> {
        let schema = r#"
            extend schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
              @link(url: "https://example.com/my-directive/v1.0", import: ["@myDirective"])

          type Query { x: Int }

            directive @myDirective on FIELD_DEFINITION | ARGUMENT_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "url_dash.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?;
        let meta = meta.expect("should have metadata");

        assert!(
            meta.source_link_of_directive(&name!("myDirective"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn computes_link_metadata() {
        let schema = r#"
          extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0", import: ["Import"])
            @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", { name: "@tag", as: "@myTag" }])
            @link(url: "https://custom.com/someSpec/v0.2", as: "mySpec")
            @link(url: "https://megacorp.com/auth/v1.0", for: SECURITY)

          type Query {
            x: Int
          }

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          scalar Import

          directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "testSchema").unwrap();

        let meta = LinksMetadata::from_schema(&schema)
            // TODO: error handling?
            .unwrap()
            .unwrap();
        let names_in_schema = meta
            .all_links()
            .map(|l| l.spec_name_in_schema())
            .collect::<Vec<_>>();
        assert_eq!(names_in_schema.len(), 4);
        assert_eq!(names_in_schema[0], "link");
        assert_eq!(names_in_schema[1], "federation");
        assert_eq!(names_in_schema[2], "mySpec");
        assert_eq!(names_in_schema[3], "auth");

        let link_spec = meta.for_identity(&Identity::link_identity()).unwrap();
        assert_eq!(
            link_spec.imports.first().unwrap().as_ref(),
            &Import {
                element: name!("Import"),
                is_directive: false,
                alias: None
            }
        );

        let fed_spec = meta.for_identity(&Identity::federation_identity()).unwrap();
        assert_eq!(fed_spec.url.version, Version { major: 2, minor: 3 });
        assert_eq!(fed_spec.purpose, None);

        let imports = &fed_spec.imports;
        assert_eq!(imports.len(), 2);
        assert_eq!(
            imports.first().unwrap().as_ref(),
            &Import {
                element: name!("key"),
                is_directive: true,
                alias: None
            }
        );
        assert_eq!(
            imports.get(1).unwrap().as_ref(),
            &Import {
                element: name!("tag"),
                is_directive: true,
                alias: Some(name!("myTag"))
            }
        );

        let auth_spec = meta
            .for_identity(&Identity {
                domain: "https://megacorp.com".to_string(),
                name: name!("auth").into(),
            })
            .unwrap();
        assert_eq!(auth_spec.purpose, Some(Purpose::SECURITY));

        let import_source = meta.source_link_of_type(&name!("Import")).unwrap();
        assert_eq!(import_source.link.url.identity.name.as_ref(), "link");
        assert_eq!(import_source.name_in_spec, Some(name!("Import")));

        // Purpose is not imported, so it should only be accessible in fql form
        assert!(meta.source_link_of_type(&name!("Purpose")).is_none());

        let purpose_source = meta.source_link_of_type(&name!("link__Purpose")).unwrap();
        assert_eq!(purpose_source.link.url.identity.name.as_ref(), "link");
        assert_eq!(purpose_source.name_in_spec, Some(name!("Purpose")));

        let key_source = meta.source_link_of_directive(&name!("key")).unwrap();
        assert_eq!(key_source.link.url.identity.name.as_ref(), "federation");
        assert_eq!(key_source.name_in_spec, Some(name!("key")));

        // tag is imported under an alias, so "tag" itself should not match
        assert!(meta.source_link_of_directive(&name!("tag")).is_none());

        let tag_source = meta.source_link_of_directive(&name!("myTag")).unwrap();
        assert_eq!(tag_source.link.url.identity.name.as_ref(), "federation");
        assert_eq!(tag_source.name_in_spec, Some(name!("tag")));
    }

    /// TODO: In the JS codebase, malformed imports can generate multiple errors, we should mirror
    ///       that behavior eventually.
    mod link_import {
        use super::*;

        #[test]
        fn errors_on_malformed_values() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [2]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "`2` in @link(import:) argument must either be a string `\"<importedElement>\"` or an object `{ name: \"<importedElement>\", as: \"<alias>\" }`",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ foo: "bar" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{foo: \"bar\"}` in @link(import:) argument, field \"foo\" is not a known field",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: "@key", badName: "foo" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{name: \"@key\", badName: \"foo\"}` in @link(import:) argument, field \"badName\" is not a known field",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: 42 }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{name: 42}` in @link(import:) argument, value for field \"name\" must be a string",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: "42" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{name: \"42\"}` in @link(import:) argument, value for field \"name\" is not a valid GraphQL name",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: "" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{name: \"\"}` in @link(import:) argument, value for field \"name\" is not a valid GraphQL name",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: "@bar", as: "@" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{name: \"@bar\", as: \"@\"}` in @link(import:) argument, value for field \"as\" is not a valid GraphQL name",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ as: "bar" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{as: \"bar\"}` in @link(import:) argument, missing required field \"name\"",
            ]
            "###);
        }

        #[test]
        fn errors_on_mismatch_between_name_and_alias() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: "@key", as: "myKey" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{name: \"@key\", as: \"myKey\"}` in @link(import:) argument, value for field \"as\" must start with \"@\" since value for field \"name\" does (\"@\" indicates a directive import)",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: "FieldSet", as: "@fieldSet" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "For `{name: \"FieldSet\", as: \"@fieldSet\"}` in @link(import:) argument, value for field \"as\" must not start with \"@\" since value for field \"name\" does not (\"@\" indicates a directive import)",
            ]
            "###);
        }

        #[test]
        fn errors_on_importing_unknown_elements_for_known_features() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: ["@foo"]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import unknown element \"@foo\".",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: ["key"]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import unknown element \"key\". Did you mean directive \"@key\"?",
            ]
            "###);
            insta::assert_debug_snapshot!(errors(r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [{ name: "@sharable" }]
              )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import unknown element \"@sharable\". Did you mean \"@shareable\"?",
            ]
            "###);
        }
    }

    mod link_alias_and_import_conflicts {
        use super::*;

        #[test]
        fn errors_for_same_identity_imported_twice() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://specs.apollo.dev/federation/v2.0")

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot link feature \"https://specs.apollo.dev/federation\" since it has already been linked in the schema.",
            ]
            "###);
        }

        #[test]
        fn errors_for_spec_name_in_schema_containing_double_underscore() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/f__oo/v1.0")

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot link feature \"https://custom.dev/f__oo\" as \"f__oo\" since it contains \"__\". Please rename to a compliant name via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_containing_double_underscore() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/f__oo/v1.0", as: "foo")

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        // See the relevant code in `LinksMetadata.add_link()` for why we have this exception. That
        // exception and this test may be removed in the future once we have dropped support for the
        // bugged compositions that necessitate the exception.
        #[test]
        fn allows_exception_in_double_underscore_validation_for_federation_namespaced_tag_and_inaccessible()
         {
            insta::assert_debug_snapshot!(errors(r#"
              schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
                @link(
                  url: "https://specs.apollo.dev/inaccessible/v0.2"
                  as: "federation__inaccessible"
                  for: SECURITY
                )
                @link(url: "https://specs.apollo.dev/tag/v0.3", as: "federation__tag") {
                query: Query
              }

              directive @federation__tag(
                name: String!
              ) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

              directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

              directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

              directive @join__field(
                graph: join__Graph
                requires: join__FieldSet
                provides: join__FieldSet
                type: String
                external: Boolean
                override: String
                usedOverridden: Boolean
              ) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

              directive @join__graph(name: String!, url: String!) on ENUM_VALUE

              directive @join__implements(
                graph: join__Graph!
                interface: String!
              ) repeatable on OBJECT | INTERFACE

              directive @join__type(
                graph: join__Graph!
                key: join__FieldSet
                extension: Boolean! = false
                resolvable: Boolean! = true
                isInterfaceObject: Boolean! = false
              ) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

              directive @join__unionMember(
                graph: join__Graph!
                member: String!
              ) repeatable on UNION

              directive @link(
                url: String
                as: String
                for: link__Purpose
                import: [link__Import]
              ) repeatable on SCHEMA

              scalar join__FieldSet

              enum join__Graph {
                S @join__graph(name: "s", url: "")
              }

              scalar link__Import

              enum link__Purpose {
                SECURITY
                EXECUTION
              }

              type Query @join__type(graph: S) {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_spec_name_in_schema_ending_in_underscore() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/foo_/v1.0")

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot link feature \"https://custom.dev/foo_\" as \"foo_\" since it ends in \"_\". Please rename to a compliant name via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_ending_in_underscore() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/foo_/v1.0", as: "foo")

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_spec_name_in_schema_that_is_not_a_valid_graphql_name() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/0foo/v1.0")

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot link feature \"https://custom.dev/0foo\" as \"0foo\" since it is not a valid GraphQL name. Please rename to a compliant name via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_that_is_not_a_valid_graphql_name() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/0foo/v1.0", as: "foo")

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        // See the relevant code in `LinksMetadata.add_link()` for why we have this exception. That
        // exception and this test may be removed in the future during a major version bump.
        #[test]
        fn allows_exception_in_graphql_name_validation_for_period_and_hyphen() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/f-o.o/v1.0")

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_spec_name_in_schema_that_conflicts_with_past_namespaced_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: [{ name: "@key", as: "@foo__key" }]
                )
                @link(url: "https://custom.dev/foo/v1.0")

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@key\" as \"@foo__key\" from feature \"https://specs.apollo.dev/federation\" since it can be confused with a namespaced name from another linked feature \"https://custom.dev/foo\". Please rename the import or feature to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_that_conflicts_with_past_namespaced_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: [{ name: "@key", as: "@foo__key" }]
                )
                @link(url: "https://custom.dev/foo/v1.0", as: "bar")

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_spec_name_in_schema_that_conflicts_with_future_namespaced_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  import: [{ name: "Foo", as: "federation__Foo" }]
                )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"Foo\" as \"federation__Foo\" from feature \"https://custom.dev/foo\" since it can be confused with a namespaced name from another linked feature \"https://specs.apollo.dev/federation\". Please rename the import or feature to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_that_conflicts_with_future_namespaced_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", as: "bar")
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  import: [{ name: "Foo", as: "federation__Foo" }]
                )

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_spec_name_in_schema_that_conflicts_with_past_default_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://custom.dev/key/v1.0")

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@key\" from feature \"https://specs.apollo.dev/federation\" since it can be confused with a namespaced name from another linked feature \"https://custom.dev/key\". Please rename the import or feature to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_that_conflicts_with_past_default_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://custom.dev/key/v1.0", as: "foo")

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_spec_name_in_schema_that_conflicts_with_future_default_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  import: [{ name: "@foo", as: "@federation" }]
                )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@foo\" as \"@federation\" from feature \"https://custom.dev/foo\" since it can be confused with a namespaced name from another linked feature \"https://specs.apollo.dev/federation\". Please rename the import or feature to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_that_conflicts_with_future_default_directive() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", as: "bar")
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  import: [{ name: "@foo", as: "@federation" }]
                )

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_spec_name_in_schema_that_conflicts_with_another_spec_name_in_schema() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/federation/v1.0")

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot link feature https://custom.dev/federation as \"federation\" since another feature \"https://specs.apollo.dev/federation\" already uses that alias. Please rename the feature to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_spec_name_that_conflicts_with_another_spec_name_in_schema() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(url: "https://custom.dev/federation/v1.0", as: "foo")

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_namespaced_import_that_is_not_a_no_op_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: [{ name: "@key", as: "@federation__requires" }]
                )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@key\" as \"@federation__requires\" from feature \"https://specs.apollo.dev/federation\" since it can be confused with the namespaced name for \"@requires\". Please rename the import to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_for_namespaced_import_that_is_a_no_op_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: [{ name: "@key", as: "@federation__key" }]
                )

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_default_directive_import_that_is_not_a_no_op_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  as: "bar"
                  import: [{ name: "@baz", as: "@bar" }]
                )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@baz\" as \"@bar\" from feature \"https://custom.dev/foo\" since it can be confused with the namespaced name for \"@foo\". Please rename the import to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_for_default_directive_import_that_is_a_no_op_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0")
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  as: "bar"
                  import: [{ name: "@foo", as: "@bar" }]
                )

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_imports_of_one_element_to_different_names() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: ["@key", { name: "@key", as: "@foo" }]
                )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@key\" as \"@foo\" from feature \"https://specs.apollo.dev/federation\" since it was previously imported as \"@key\". Please remove one of these imports.",
            ]
            "###);
        }

        #[test]
        fn succeeds_for_imports_of_one_element_to_same_name() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: [
                    { name: "@key", as: "@foo" }
                    "@requires"
                    { name: "@key", as: "@foo" }
                  ]
                )

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_import_name_in_schema_that_already_exists_in_different_spec() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  import: [{ name: "@foo", as: "@key" }]
                )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@foo\" as \"@key\" from feature \"https://custom.dev/foo\" since it was previously imported from feature \"https://specs.apollo.dev/federation\". Please rename the import to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_import_name_that_already_exists_in_different_spec() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: [{ name: "@key", as: "@bar" }]
                )
                @link(
                  url: "https://custom.dev/foo/v1.0"
                  import: [{ name: "@foo", as: "@key" }]
                )

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_import_name_in_schema_that_already_exists_in_same_spec() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: ["@key", { name: "@requires", as: "@key" }]
                )

              type Query {
                q: Int
              }
            "#), @r###"
            [
                "Cannot import \"@requires\" as \"@key\" from feature \"https://specs.apollo.dev/federation\" since it was previously imported for \"@key\". Please rename the import to avoid conflicts via \"as\".",
            ]
            "###);
        }

        #[test]
        fn succeeds_renaming_import_name_that_already_exists_in_same_spec() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: [
                    { name: "@key", as: "@requires" }
                    { name: "@requires", as: "@key" }
                  ]
                )

              type Query {
                q: Int
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_used_shadowed_directive_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

              directive @federation__key(
                fields: federation__FieldSet!
                resolvable: Boolean = true
              ) repeatable on OBJECT | INTERFACE

              scalar federation__FieldSet

              type Query {
                users: [User!]!
              }

              type User @federation__key(fields: "id") {
                id: ID!
                name: String!
              }
            "#), @r###"
            [
                "Cannot import \"@key\" from feature \"https://specs.apollo.dev/federation\" since there's a used definition for the namespaced name \"@federation__key\". Please switch usages of the namespaced name to the import name and remove the definition.",
            ]
            "###);
        }

        #[test]
        fn succeeds_for_unused_shadowed_directive_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

              directive @federation__key(
                fields: federation__FieldSet!
                resolvable: Boolean = true
              ) repeatable on OBJECT | INTERFACE

              scalar federation__FieldSet

              type Query {
                users: [User!]!
              }

              type User @key(fields: "id") {
                id: ID!
                name: String!
              }
            "#), @"[]");
        }

        #[test]
        fn errors_for_used_shadowed_type_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: ["FieldSet"]
                )

              scalar federation__FieldSet

              type Query {
                users: [User!]!
              }

              type User {
                id: ID!
                fieldSet: federation__FieldSet!
              }
            "#), @r###"
            [
                "Cannot import \"FieldSet\" from feature \"https://specs.apollo.dev/federation\" since there's a used definition for the namespaced name \"federation__FieldSet\". Please switch usages of the namespaced name to the import name and remove the definition.",
            ]
            "###);
        }

        #[test]
        fn succeeds_for_unused_shadowed_type_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: ["FieldSet"]
                )

              scalar federation__FieldSet

              type Query {
                users: [User!]!
              }

              type User {
                id: ID!
                fieldSet: String!
              }
            "#), @"[]");
        }

        #[test]
        fn succeeds_for_shadowed_type_import_used_in_shadowed_import() {
            insta::assert_debug_snapshot!(errors(r#"
              extend schema
                @link(
                  url: "https://specs.apollo.dev/federation/v2.0"
                  import: ["@key", "FieldSet"]
                )

              directive @federation__key(
                fields: federation__FieldSet!
                resolvable: Boolean = true
              ) repeatable on OBJECT | INTERFACE

              scalar federation__FieldSet

              type Query {
                users: [User!]!
              }

              type User {
                id: ID!
                fieldSet: String!
              }
            "#), @"[]");
        }
    }

    #[test]
    fn allowed_link_directive_definitions() -> Result<(), FederationError> {
        let link_defs = [
            "directive @link(url: String!, as: String) repeatable on SCHEMA",
            "directive @link(url: String, as: String) repeatable on SCHEMA",
            "directive @link(url: String!) repeatable on SCHEMA",
            "directive @link(url: String) repeatable on SCHEMA",
        ];
        let schema_prefix = r#"
          extend schema @link(url: "https://specs.apollo.dev/link/v1.0")
          type Query { x: Int }
        "#;
        for link_def in link_defs {
            let schema_doc = format!("{schema_prefix}\n{link_def}");
            let schema = Schema::parse(&schema_doc, "test.graphql").unwrap();
            let meta = LinksMetadata::from_schema(&schema)?;
            assert!(meta.is_some(), "should have metadata for: {link_def}");
        }
        Ok(())
    }
}
