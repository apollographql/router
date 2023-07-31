### Configure AWS sigv4 authentication for subgraph requests ([PR #3365](https://github.com/apollographql/router/pull/3365))

Secure your router to subgraph communication on AWS by using Sigv4!
This changeset provides you with a way to set up Hardcoded credentials, as well as a Default provider chain.
We recommend using the DefaultChain configuration.

Full use example:

```yaml
    authentication:
      subgraph:
        all: # configuration that will apply to all subgraphs
          aws_sig_v4:
            default_chain:
              profile_name: "my-test-profile" # https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/iam-roles-for-amazon-ec2.html#ec2-instance-profile
              region: "us-east-1" # https://docs.aws.amazon.com/general/latest/gr/rande.html
              service_name: "lambda" # https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_aws-services-that-work-with-iam.html
              assume_role: # https://docs.aws.amazon.com/IAM/latest/UserGuide/id_roles.html
                role_arn: "test-arn"
                session_name: "test-session"
                external_id: "test-id"
        subgraphs:
          products:
            aws_sig_v4:
              hardcoded: # Not recommended, prefer using default_chain as shown above
                access_key_id: "my-access-key"
                secret_access_key: "my-secret-access-key"
                region: "us-east-1"
                service_name: "vpc-lattice-svcs" # "s3", "lambda" etc.
```

The full documentation can be found in the [router documentation](https://www.apollographql.com/docs/router/configuration/authn-subgraph)

By [@o0Ignition0o](https://github.com/o0Ignition0o) and [@BlenderDude](https://github.com/BlenderDude) in https://github.com/apollographql/router/pull/3365
