package finance.router

import rego.v1

authorize := {
    "contract": input.contract,
    "decisions": {policy: allowed(policy) | some policy in input.policies},
}

allowed(policy) := policy == "finance:approve_refund"
