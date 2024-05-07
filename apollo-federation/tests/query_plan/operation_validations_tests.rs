///
/// validations
///

#[test]
fn reject_defer_on_mutation() {
    //test.each([
    //     { directive: '@defer', rootKind: 'mutation' },
    //     { directive: '@defer', rootKind: 'subscription' },
    //     { directive: '@stream', rootKind: 'mutation' },
    //     { directive: '@stream', rootKind: 'subscription' },
    //   ])('reject $directive on $rootKind type', ({ directive, rootKind }) => {
    //     const schema = parseSchema(`
    //       type Query {
    //         x: String
    //       }
    //
    //       type Mutation {
    //         x: String
    //       }
    //
    //       type Subscription {
    //         x: String
    //       }
    //     `);
    //
    //     expect(() => {
    //       parseOperation(schema, `
    //         ${rootKind} {
    //           ... ${directive} {
    //             x
    //           }
    //         }
    //       `)
    //     }).toThrowError(new GraphQLError(`The @defer and @stream directives cannot be used on ${rootKind} root type "${defaultRootName(rootKind as SchemaRootKind)}"`));
    //   });
}

#[test]
fn reject_defer_on_subscription() {
    // see reject_defer_on_mutation
}

#[test]
fn reject_stream_on_mutation() {
    // see reject_defer_on_mutation
}

#[test]
fn reject_stream_on_subscription() {
    //// see reject_defer_on_mutation
}

///
/// conflicts
///

#[test]
fn conflict_between_selection_and_reused_fragment() {
    //test('due to conflict between selection and reused fragment', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: T1
    //           i: I
    //         }
    //
    //         interface I {
    //           id: ID!
    //         }
    //
    //         interface WithF {
    //           f(arg: Int): Int
    //         }
    //
    //         type T1 implements I {
    //           id: ID!
    //           f(arg: Int): Int
    //         }
    //
    //         type T2 implements I & WithF {
    //           id: ID!
    //           f(arg: Int): Int
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         query {
    //           t1 {
    //             id
    //             f(arg: 0)
    //           }
    //           i {
    //             ...F1
    //           }
    //         }
    //
    //         fragment F1 on I {
    //           id
    //           ... on WithF {
    //             f(arg: 1)
    //           }
    //         }
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           t1 {
    //             id
    //             f(arg: 0)
    //           }
    //           i {
    //             id
    //             ... on WithF {
    //               f(arg: 1)
    //             }
    //           }
    //         }
    //       `);
    //
    //       // Note that technically, `t1` has return type `T1` which is a `I`, so `F1` can be spread
    //       // within `t1`, and `t1 { ...F1 }` is just `t1 { id }` (because `T!` does not implement `WithF`),
    //       // so that it would appear that it could be valid to optimize this query into:
    //       //   {
    //       //     t1 {
    //       //       ...F1       // Notice the use of F1 here, which does expand to `id` in this context
    //       //       f(arg: 0)
    //       //     }
    //       //     i {
    //       //       ...F1
    //       //     }
    //       //   }
    //       // And while doing this may look "dumb" in that toy example (we're replacing `id` with `...F1`
    //       // which is longer so less optimal really), it's easy to expand this to example where re-using
    //       // `F1` this way _does_ make things smaller.
    //       //
    //       // But the query above is actually invalid. And it is invalid because the validation of graphQL
    //       // does not take into account the fact that the `... on WithF` part of `F1` is basically dead
    //       // code within `t1`. And so it finds a conflict between `f(arg: 0)` and the `f(arg: 1)` in `F1`
    //       // (even though, again, the later is statically known to never apply, but graphQL does not
    //       // include such static analysis in its validation).
    //       //
    //       // And so this test does make sure we do not generate the query above (do not use `F1` in `t1`).
    //       const optimized = withoutFragments.optimize(operation.fragments!, 1);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment F1 on I {
    //           id
    //           ... on WithF {
    //             f(arg: 1)
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             id
    //             f(arg: 0)
    //           }
    //           i {
    //             ...F1
    //           }
    //         }
    //       `);
    //     });
}

#[test]
fn conflict_between_reused_fragment_and_another_trimmed_fragment() {
    //test('due to conflict between the active selection of a reused fragment and the trimmed part of another fragments', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: T1
    //           i: I
    //         }
    //
    //         interface I {
    //           id: ID!
    //         }
    //
    //         interface WithF {
    //           f(arg: Int): Int
    //         }
    //
    //         type T1 implements I {
    //           id: ID!
    //           f(arg: Int): Int
    //         }
    //
    //         type T2 implements I & WithF {
    //           id: ID!
    //           f(arg: Int): Int
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         query {
    //           t1 {
    //             id
    //             ...F1
    //           }
    //           i {
    //             ...F2
    //           }
    //         }
    //
    //         fragment F1 on T1 {
    //           f(arg: 0)
    //         }
    //
    //         fragment F2 on I {
    //           id
    //           ... on WithF {
    //             f(arg: 1)
    //           }
    //         }
    //
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           t1 {
    //             id
    //             f(arg: 0)
    //           }
    //           i {
    //             id
    //             ... on WithF {
    //               f(arg: 1)
    //             }
    //           }
    //         }
    //       `);
    //
    //       // See the comments on the previous test. The only different here is that `F1` is applied
    //       // first, and then we need to make sure we do not apply `F2` even though it's restriction
    //       // inside `t1` matches its selection set.
    //       const optimized = withoutFragments.optimize(operation.fragments!, 1);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment F1 on T1 {
    //           f(arg: 0)
    //         }
    //
    //         fragment F2 on I {
    //           id
    //           ... on WithF {
    //             f(arg: 1)
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             ...F1
    //             id
    //           }
    //           i {
    //             ...F2
    //           }
    //         }
    //       `);
    //     });
}

#[test]
fn conflict_between_trimmed_parts_of_two_fragments() {
    //test('due to conflict between the trimmed parts of 2 fragments', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: T1
    //           i1: I
    //           i2: I
    //         }
    //
    //         interface I {
    //           id: ID!
    //           a: Int
    //           b: Int
    //         }
    //
    //         interface WithF {
    //           f(arg: Int): Int
    //         }
    //
    //         type T1 implements I {
    //           id: ID!
    //           a: Int
    //           b: Int
    //           f(arg: Int): Int
    //         }
    //
    //         type T2 implements I & WithF {
    //           id: ID!
    //           a: Int
    //           b: Int
    //           f(arg: Int): Int
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         query {
    //           t1 {
    //             id
    //             a
    //             b
    //           }
    //           i1 {
    //             ...F1
    //           }
    //           i2 {
    //             ...F2
    //           }
    //         }
    //
    //         fragment F1 on I {
    //           id
    //           a
    //           ... on WithF {
    //             f(arg: 0)
    //           }
    //         }
    //
    //         fragment F2 on I {
    //           id
    //           b
    //           ... on WithF {
    //             f(arg: 1)
    //           }
    //         }
    //
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           t1 {
    //             id
    //             a
    //             b
    //           }
    //           i1 {
    //             id
    //             a
    //             ... on WithF {
    //               f(arg: 0)
    //             }
    //           }
    //           i2 {
    //             id
    //             b
    //             ... on WithF {
    //               f(arg: 1)
    //             }
    //           }
    //         }
    //       `);
    //
    //       // Here, `F1` in `T1` reduces to `{ id a }` and F2 reduces to `{ id b }`, so theoretically both could be used
    //       // within the first `T1` branch. But they can't both be used because their `... on WithF` part conflict,
    //       // and even though that part is dead in `T1`, this would still be illegal graphQL.
    //       const optimized = withoutFragments.optimize(operation.fragments!, 1);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment F1 on I {
    //           id
    //           a
    //           ... on WithF {
    //             f(arg: 0)
    //           }
    //         }
    //
    //         fragment F2 on I {
    //           id
    //           b
    //           ... on WithF {
    //             f(arg: 1)
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             ...F1
    //             b
    //           }
    //           i1 {
    //             ...F1
    //           }
    //           i2 {
    //             ...F2
    //           }
    //         }
    //       `);
    //     });
}

#[test]
fn conflict_between_selection_and_reused_fragment_at_different_level() {
    // test('due to conflict between selection and reused fragment at different levels', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: SomeV
    //           t2: SomeV
    //         }
    //
    //         union SomeV = V1 | V2 | V3
    //
    //         type V1 {
    //           x: String
    //         }
    //
    //         type V2 {
    //           y: String!
    //         }
    //
    //         type V3 {
    //           x: Int
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         fragment onV1V2 on SomeV {
    //           ... on V1 {
    //             x
    //           }
    //           ... on V2 {
    //             y
    //           }
    //         }
    //
    //         query {
    //           t1 {
    //             ...onV1V2
    //           }
    //           t2 {
    //             ... on V2 {
    //               y
    //             }
    //             ... on V3 {
    //               x
    //             }
    //           }
    //         }
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           t1 {
    //             ... on V1 {
    //               x
    //             }
    //             ... on V2 {
    //               y
    //             }
    //           }
    //           t2 {
    //             ... on V2 {
    //               y
    //             }
    //             ... on V3 {
    //               x
    //             }
    //           }
    //         }
    //       `);
    //
    //       const optimized = withoutFragments.optimize(operation.fragments!, 1);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment onV1V2 on SomeV {
    //           ... on V1 {
    //             x
    //           }
    //           ... on V2 {
    //             y
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             ...onV1V2
    //           }
    //           t2 {
    //             ... on V2 {
    //               y
    //             }
    //             ... on V3 {
    //               x
    //             }
    //           }
    //         }
    //       `);
    //     });
}

#[test]
fn conflict_between_fragments_at_different_levels() {
    //test('due to conflict between the trimmed parts of 2 fragments at different levels', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: SomeV
    //           t2: SomeV
    //           t3: OtherV
    //         }
    //
    //         union SomeV = V1 | V2 | V3
    //         union OtherV = V3
    //
    //         type V1 {
    //           x: String
    //         }
    //
    //         type V2 {
    //           x: Int
    //         }
    //
    //         type V3 {
    //           y: String!
    //           z: String!
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         fragment onV1V3 on SomeV {
    //           ... on V1 {
    //             x
    //           }
    //           ... on V3 {
    //             y
    //           }
    //         }
    //
    //         fragment onV2V3 on SomeV {
    //           ... on V2 {
    //             x
    //           }
    //           ... on V3 {
    //             z
    //           }
    //         }
    //
    //         query {
    //           t1 {
    //             ...onV1V3
    //           }
    //           t2 {
    //             ...onV2V3
    //           }
    //           t3 {
    //             ... on V3 {
    //               y
    //               z
    //             }
    //           }
    //         }
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           t1 {
    //             ... on V1 {
    //               x
    //             }
    //             ... on V3 {
    //               y
    //             }
    //           }
    //           t2 {
    //             ... on V2 {
    //               x
    //             }
    //             ... on V3 {
    //               z
    //             }
    //           }
    //           t3 {
    //             ... on V3 {
    //               y
    //               z
    //             }
    //           }
    //         }
    //       `);
    //
    //       const optimized = withoutFragments.optimize(operation.fragments!, 1);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment onV1V3 on SomeV {
    //           ... on V1 {
    //             x
    //           }
    //           ... on V3 {
    //             y
    //           }
    //         }
    //
    //         fragment onV2V3 on SomeV {
    //           ... on V2 {
    //             x
    //           }
    //           ... on V3 {
    //             z
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             ...onV1V3
    //           }
    //           t2 {
    //             ...onV2V3
    //           }
    //           t3 {
    //             ...onV1V3
    //             ... on V3 {
    //               z
    //             }
    //           }
    //         }
    //       `);
    //     });
}

#[test]
fn conflict_between_two_sibling_branches() {
    // test('due to conflict between 2 sibling branches', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: SomeV
    //           i: I
    //         }
    //
    //         interface I {
    //           id: ID!
    //         }
    //
    //         type T1 implements I {
    //           id: ID!
    //           t2: SomeV
    //         }
    //
    //         type T2 implements I {
    //           id: ID!
    //           t2: SomeV
    //         }
    //
    //         union SomeV = V1 | V2 | V3
    //
    //         type V1 {
    //           x: String
    //         }
    //
    //         type V2 {
    //           y: String!
    //         }
    //
    //         type V3 {
    //           x: Int
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         fragment onV1V2 on SomeV {
    //           ... on V1 {
    //             x
    //           }
    //           ... on V2 {
    //             y
    //           }
    //         }
    //
    //         query {
    //           t1 {
    //             ...onV1V2
    //           }
    //           i {
    //             ... on T1 {
    //               t2 {
    //                 ... on V2 {
    //                   y
    //                 }
    //               }
    //             }
    //             ... on T2 {
    //               t2 {
    //                 ... on V3 {
    //                   x
    //                 }
    //               }
    //             }
    //           }
    //         }
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           t1 {
    //             ... on V1 {
    //               x
    //             }
    //             ... on V2 {
    //               y
    //             }
    //           }
    //           i {
    //             ... on T1 {
    //               t2 {
    //                 ... on V2 {
    //                   y
    //                 }
    //               }
    //             }
    //             ... on T2 {
    //               t2 {
    //                 ... on V3 {
    //                   x
    //                 }
    //               }
    //             }
    //           }
    //         }
    //       `);
    //
    //       const optimized = withoutFragments.optimize(operation.fragments!, 1);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment onV1V2 on SomeV {
    //           ... on V1 {
    //             x
    //           }
    //           ... on V2 {
    //             y
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             ...onV1V2
    //           }
    //           i {
    //             ... on T1 {
    //               t2 {
    //                 ... on V2 {
    //                   y
    //                 }
    //               }
    //             }
    //             ... on T2 {
    //               t2 {
    //                 ... on V3 {
    //                   x
    //                 }
    //               }
    //             }
    //           }
    //         }
    //       `);
    //     });
}

#[test]
fn conflict_when_inline_fragment_should_be_normalized() {
    //  test('when a spread inside an expanded fragment should be "normalized away"', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: T1
    //           i: I
    //         }
    //
    //         interface I {
    //           id: ID!
    //         }
    //
    //         type T1 implements I {
    //           id: ID!
    //           a: Int
    //         }
    //
    //         type T2 implements I {
    //           id: ID!
    //           b: Int
    //           c: Int
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         {
    //           t1 {
    //             ...GetAll
    //           }
    //           i {
    //             ...GetT2
    //           }
    //         }
    //
    //         fragment GetAll on I {
    //            ... on T1 {
    //              a
    //            }
    //            ...GetT2
    //            ... on T2 {
    //              c
    //            }
    //         }
    //
    //         fragment GetT2 on T2 {
    //            b
    //         }
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           t1 {
    //             a
    //           }
    //           i {
    //             ... on T2 {
    //               b
    //             }
    //           }
    //         }
    //       `);
    //
    //       // As we re-optimize, we will initially generated the initial query. But
    //       // as we ask to only optimize fragments used more than once, the `GetAll`
    //       // fragment will be re-expanded (`GetT2` will not because the code will say
    //       // that it is used both in the expanded `GetAll` but also inside `i`).
    //       // But because `GetAll` is within `t1: T1`, that expansion should actually
    //       // get rid of anything `T2`-related.
    //       // This test exists because a previous version of the code was not correctly
    //       // "getting rid" of the `...GetT2` spread, keeping in the query, which is
    //       // invalid (we cannot have `...GetT2` inside `t1`).
    //       const optimized = withoutFragments.optimize(operation.fragments!, 2);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment GetT2 on T2 {
    //           b
    //         }
    //
    //         {
    //           t1 {
    //             a
    //           }
    //           i {
    //             ...GetT2
    //           }
    //         }
    //       `);
    //     });
}

#[test]
fn conflict_due_to_trimmed_selections_of_nested_fragments() {
    //test('due to the trimmed selection of nested fragments', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           u1: U
    //           u2: U
    //           u3: U
    //         }
    //
    //         union U = S | T
    //
    //         type T  {
    //           id: ID!
    //           vt: Int
    //         }
    //
    //         interface I {
    //           vs: Int
    //         }
    //
    //         type S implements I {
    //           vs: Int!
    //         }
    //       `);
    //       const gqlSchema = schema.toGraphQLJSSchema();
    //
    //       const operation = parseOperation(schema, `
    //         {
    //           u1 {
    //             ...F1
    //           }
    //           u2 {
    //             ...F3
    //           }
    //           u3 {
    //             ...F3
    //           }
    //         }
    //
    //         fragment F1 on U {
    //            ... on S {
    //              __typename
    //              vs
    //            }
    //            ... on T {
    //              __typename
    //              vt
    //            }
    //         }
    //
    //         fragment F2 on T {
    //            __typename
    //            vt
    //         }
    //
    //         fragment F3 on U {
    //            ... on I {
    //              vs
    //            }
    //            ...F2
    //         }
    //       `);
    //       expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //       const withoutFragments = operation.expandAllFragments();
    //       expect(withoutFragments.toString()).toMatchString(`
    //         {
    //           u1 {
    //             ... on S {
    //               __typename
    //               vs
    //             }
    //             ... on T {
    //               __typename
    //               vt
    //             }
    //           }
    //           u2 {
    //             ... on I {
    //               vs
    //             }
    //             ... on T {
    //               __typename
    //               vt
    //             }
    //           }
    //           u3 {
    //             ... on I {
    //               vs
    //             }
    //             ... on T {
    //               __typename
    //               vt
    //             }
    //           }
    //         }
    //       `);
    //
    //       // We use `mapToExpandedSelectionSets` with a no-op mapper because this will still expand the selections
    //       // and re-optimize them, which 1) happens to match what happens in the query planner and 2) is necessary
    //       // for reproducing a bug that this test was initially added to cover.
    //       const newFragments = operation.fragments!.mapToExpandedSelectionSets((s) => s);
    //       const optimized = withoutFragments.optimize(newFragments, 2);
    //       expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //       expect(optimized.toString()).toMatchString(`
    //         fragment F3 on U {
    //           ... on I {
    //             vs
    //           }
    //           ... on T {
    //             __typename
    //             vt
    //           }
    //         }
    //
    //         {
    //           u1 {
    //             ... on S {
    //               __typename
    //               vs
    //             }
    //             ... on T {
    //               __typename
    //               vt
    //             }
    //           }
    //           u2 {
    //             ...F3
    //           }
    //           u3 {
    //             ...F3
    //           }
    //         }
    //       `);
    //     });
}
