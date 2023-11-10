# Title [ADR-2]

Router customization mechanisms and hierarchy

## Status

Approved

## Context

When considering how to customize the behaviour of a router instance, it's not always clear which mechanism to use. This can lead to confusion for customers, since it would be nice to make these kinds of decisions in an informed fashion.

There are, effectively, three classes of customization:

 - Runtime customizations which work within a "stock" router binary
 - Rust plugin customizations which result in a modified router binary
 - Rust source customizations which also result in a modified router binary

You can consider the list above as a hierarchy of customization, with entries earlier in the list being preferred.

### Runtime customizations

Currently we have three forms of runtime customizations:

 - Configuration
 - Rhai scripting
 - Coprocessor calls

You can consider the list above as a hierarchy of customization, with entries earlier in the list being preferred.

All runtime customizations are preferred over source customizations, since the benefits of using the "stock" router binary in terms of known performance, security, etc... are significant.

#### Configuration

The router may be customized by providing YAML configuration. This is the simplest form of customization and has the greatest impact. Performance impact is minimal and security is the highest.

#### Rhai scripting

Rhai scripts execute inside a constrained sandbox. This means that they are very secure. Also, because they are executing within the router process, the performance impact is minimal.

#### Coprocessor

The router serializes data and provides it to a coprocessor via an HTTP POST with a JSON payload. Security must be managed by the operators of the route and performance is impacted when sending data and receiving responses.

Coprocessor functionality is currently a commercial-only feature of the router.

### Plugin customizations

A new router is built which executes within the constraints of the router's plugin framework. This defines hook points where Rust source code can be compiled in with Router source code and custom binaries built to solve problems which aren't possible via runtime customization. The main disadvantage of this approach is that the customization is no longer making use of the "stock" router binary.

### Source customizations

Any changes can be made to the router since it is open source code. There are no technical limiations, but clearly this isn't making use of the "stock" router binary.

## Decision

This DR proposes that we formally adopt this customization model and make it clear to users how we prefer to provide customization facilities to users. It further proposes that we endeavour to "promote" common customizations up the hierarchy towards configuration whereever it is technically possible.

We should analyze available data about customization mechanisms in use (and follow up with customers where appropriate) to decide how best to improve existing features.

## Consequences

It should be clearer for developers that adding customization mechanisms is preferable to modifying the source code of the router.

It should be clearer to customers what the pros and cons of using various customization mechanisms are.

In future, if we consider adding additional forms of customization, the existence of this hierarchy will make it easier to decide where such a new mechanism may position itself in relation to existing mechanisms.

## Addendum

Customization forms that have been discussed in the past include:

 - Binary Plugins as Shared Objects
 - Commercial Rhai Modules
 - WASM support

