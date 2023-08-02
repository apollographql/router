use std::collections::{BTreeMap, BTreeSet};

use displaydoc::Display;
use serde_json::Value;
use thiserror::Error;

use super::ClaimConf;

#[derive(Debug, Display, Error)]

pub(super) enum Error {
    /// claim '{claim}' must be present
    Absent { claim: String },
    /// Invalid value for claim '{claim}': expected '{expected}', got '{value}'
    NotEqual {
        claim: String,
        value: Value,
        expected: Value,
    },
    /// Invalid value for claim '{claim}': '{value}' is not a string
    NotAString { claim: String, value: Value },
    /// Invalid value for claim '{claim}': '{value}' is not in the set of accepted strings {set:?}
    NotInSet {
        claim: String,
        value: String,
        set: BTreeSet<String>,
    },
}

pub(super) fn check_claims(
    config: &BTreeMap<String, ClaimConf>,
    claims: &Value,
) -> Result<(), Vec<Error>> {
    let mut errors = vec![];

    for (name, condition) in config {
        if let Err(e) = check_claim(name, condition, claims) {
            errors.push(e);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub(super) fn check_claim(name: &str, condition: &ClaimConf, claims: &Value) -> Result<(), Error> {
    let claim_value = claims.get(name);

    match condition {
        ClaimConf::Present => match claim_value {
            None => Err(Error::Absent { claim: name.into() }),
            Some(_) => Ok(()),
        },
        ClaimConf::Is(requested_value) => match claim_value {
            None => Err(Error::Absent { claim: name.into() }),
            Some(v) => {
                if requested_value != v {
                    Err(Error::NotEqual {
                        claim: name.into(),
                        value: v.clone(),
                        expected: requested_value.clone(),
                    })
                } else {
                    Ok(())
                }
            }
        },
        ClaimConf::OneOf(set) => match claim_value {
            None => Err(Error::Absent { claim: name.into() }),
            Some(v) => match v.as_str() {
                None => Err(Error::NotAString {
                    claim: name.into(),
                    value: v.clone(),
                }),
                Some(s) => {
                    if !set.contains(s) {
                        Err(Error::NotInSet {
                            claim: name.into(),
                            value: s.into(),
                            set: set.clone(),
                        })
                    } else {
                        Ok(())
                    }
                }
            },
        },
    }
}
