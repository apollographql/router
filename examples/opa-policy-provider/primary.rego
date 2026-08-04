package apollo.router

import rego.v1

authorize := {
    "contract": input.contract,
    "decisions": {policy: allowed(policy) | some policy in input.policies},
}

allowed(policy) := policy in {"read_profile", "read_credit_card"}
