# OPA policy provider example

This example runs two equivalent Open Policy Agent (OPA) replicas for the `primary` policy provider and one distinct `finance` provider. The policies implement the Router's `apollo.router.policy/v1` contract.

## Start OPA

From the repository root, start each command in a separate terminal:

```bash title="Primary replica 1"
docker run --rm -p 18181:8181 \
  -v "$PWD/examples/opa-policy-provider/primary.rego:/policy.rego:ro" \
  openpolicyagent/opa:latest run --server --addr=0.0.0.0:8181 /policy.rego
```

```bash title="Primary replica 2"
docker run --rm -p 18182:8181 \
  -v "$PWD/examples/opa-policy-provider/primary.rego:/policy.rego:ro" \
  openpolicyagent/opa:latest run --server --addr=0.0.0.0:8181 /policy.rego
```

```bash title="Finance provider"
docker run --rm -p 18281:8181 \
  -v "$PWD/examples/opa-policy-provider/finance.rego:/policy.rego:ro" \
  openpolicyagent/opa:latest run --server --addr=0.0.0.0:8181 /policy.rego
```

## Configure the Router

Configure the first two URLs as replicas of one logical provider. Configure the finance service as a separate provider and route the `finance:` policy namespace to it:

```yaml title="router.yaml"
authorization:
  policy:
    enabled: true
    providers:
      primary:
        type: opa
        api:
          decision: apollo/router/authorize
        endpoints:
          - url: http://127.0.0.1:18181
          - url: http://127.0.0.1:18182
        input:
          claims:
            include: []
      finance:
        type: opa
        api:
          decision: finance/router/authorize
        endpoints:
          - url: http://127.0.0.1:18281
        input:
          claims:
            include: []
    routing:
      default:
        provider: primary
      rules:
        - match:
            prefix: ["finance:"]
          target:
            provider: finance
```

The `primary` policy allows `read_profile` and `read_credit_card`; it denies other labels. The finance policy allows `finance:approve_refund`.

## Validate the example

With all three OPA servers running, run the ignored integration test from the repository root:

```bash
cargo test -p apollo-router \
  plugins::authorization::provider::tests::validates_multiple_real_opa_services \
  --lib -- --ignored
```

The test evaluates `read_profile`, an unknown primary policy, and `finance:approve_refund` twice. It exercises the configured round-robin path, confirms that the `finance:` label reaches the finance provider, and verifies that an unknown label is denied.
