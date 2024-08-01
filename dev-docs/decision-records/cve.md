# Title [ADR-5]

# CVE

## Status

Proposed

## Context

Deciding if a bug is CVE worthy needs definition so that we can be consistent.

A CVE will result in the following being triggered:
1. The creation of the CVE itself.
2. Apollo support will reach out to all customers giving with a CVE giving them a heads up.
3. A retro to discuss how the CVE happened in the first place, and how it was handled.

The Router is a critical piece of infrastructure for our customers, and we need to be careful about how we handle security issues.
However, at this time we still need to be pragmatic about what we consider CVE worthy. In particular, while denial of service is a top priority, in practice it is not possible to prevent these right now.

## Decision

Use the following criteria to decide if an issue is CVE worthy:

### CVE
Something that leaks user data that they were not expecting to:
* clients
* logs
* metrics

### NOT CVE
Something that causes a performance issue on the router, or a denial of service triggered by:
* large queries
* out of memory
* bugs in router code or dependencies
* bugs in user code

## Consequences

A CVE will be created for issues that meet the criteria. We will review this decision record after further hardening has taken place and we are confident that the attack vectors for denial of service have been removed.
We will create a plan for when this will happen and share with our users by Feb 2024.
