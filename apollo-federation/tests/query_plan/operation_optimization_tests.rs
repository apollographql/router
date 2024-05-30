#[test]
fn handles_nested_fragments_with_field_intersection() {
    //test('handles nested fragments with field intersection', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t: T
    //       }
    //
    //       type T {
    //         a: A
    //         b: Int
    //       }
    //
    //       type A {
    //         x: String
    //         y: String
    //         z: String
    //       }
    //     `);
    //
    //
    //     // The subtlety here is that `FA` contains `__typename` and so after we're reused it, the
    //     // selection will look like:
    //     // {
    //     //   t {
    //     //     a {
    //     //       ...FA
    //     //     }
    //     //   }
    //     // }
    //     // But to recognize that `FT` can be reused from there, we need to be able to see that
    //     // the `__typename` that `FT` wants is inside `FA` (and since FA applies on the parent type `A`
    //     // directly, it is fine to reuse).
    //     testFragmentsRoundtrip({
    //       schema,
    //       query:  `
    //         fragment FA on A {
    //           __typename
    //           x
    //           y
    //         }
    //
    //         fragment FT on T {
    //           a {
    //             __typename
    //             ...FA
    //           }
    //         }
    //
    //         query {
    //           t {
    //             ...FT
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           t {
    //             a {
    //               __typename
    //               x
    //               y
    //             }
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn handles_fragment_matching_subset_of_field_selection() {
    // test('handles fragment matching subset of field selection', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t: T
    //       }
    //
    //       type T {
    //         a: String
    //         b: B
    //         c: Int
    //         d: D
    //       }
    //
    //       type B {
    //         x: String
    //         y: String
    //       }
    //
    //       type D {
    //         m: String
    //         n: String
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       query: `
    //         fragment FragT on T {
    //           b {
    //             __typename
    //             x
    //           }
    //           c
    //           d {
    //             m
    //           }
    //         }
    //
    //         {
    //           t {
    //             ...FragT
    //             d {
    //               n
    //             }
    //             a
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           t {
    //             b {
    //               __typename
    //               x
    //             }
    //             c
    //             d {
    //               m
    //               n
    //             }
    //             a
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn handles_fragment_matching_subset_of_inline_fragment_selection() {
    // test('handles fragment matching subset of inline fragment selection', () => {
    //     // Pretty much the same test than the previous one, but matching inside a fragment selection inside
    //     // of inside a field selection.
    //     const schema = parseSchema(`
    //       type Query {
    //         i: I
    //       }
    //
    //       interface I {
    //         a: String
    //       }
    //
    //       type T {
    //         a: String
    //         b: B
    //         c: Int
    //         d: D
    //       }
    //
    //       type B {
    //         x: String
    //         y: String
    //       }
    //
    //       type D {
    //         m: String
    //         n: String
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       query: `
    //         fragment FragT on T {
    //           b {
    //             __typename
    //             x
    //           }
    //           c
    //           d {
    //             m
    //           }
    //         }
    //
    //         {
    //           i {
    //             ... on T {
    //               ...FragT
    //               d {
    //                 n
    //               }
    //               a
    //             }
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           i {
    //             ... on T {
    //               b {
    //                 __typename
    //                 x
    //               }
    //               c
    //               d {
    //                 m
    //                 n
    //               }
    //               a
    //             }
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn intersecting_fragments() {
    // test('intersecting fragments', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t: T
    //       }
    //
    //       type T {
    //         a: String
    //         b: B
    //         c: Int
    //         d: D
    //       }
    //
    //       type B {
    //         x: String
    //         y: String
    //       }
    //
    //       type D {
    //         m: String
    //         n: String
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       // Note: the code that reuse fragments iterates on fragments in the order they are defined in the document, but when it reuse
    //       // a fragment, it puts it at the beginning of the selection (somewhat random, it just feel often easier to read), so the net
    //       // effect on this example is that `Frag2`, which will be reused after `Frag1` will appear first in the re-optimized selection.
    //       // So we put it first in the input too so that input and output actually match (the `testFragmentsRoundtrip` compares strings,
    //       // so it is sensible to ordering; we could theoretically use `Operation.equals` instead of string equality, which wouldn't
    //       // really on ordering, but `Operation.equals` is not entirely trivial and comparing strings make problem a bit more obvious).
    //       query: `
    //         fragment Frag1 on T {
    //           b {
    //             x
    //           }
    //           c
    //           d {
    //             m
    //           }
    //         }
    //
    //         fragment Frag2 on T {
    //           a
    //           b {
    //             __typename
    //             x
    //           }
    //           d {
    //             m
    //             n
    //           }
    //         }
    //
    //         {
    //           t {
    //             ...Frag1
    //             ...Frag2
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           t {
    //             b {
    //               x
    //               __typename
    //             }
    //             c
    //             d {
    //               m
    //               n
    //             }
    //             a
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn fragments_application_makes_type_condition_trivial() {
    //  test('fragments whose application makes a type condition trivial', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t: T
    //       }
    //
    //       interface I {
    //         x: String
    //       }
    //
    //       type T implements I {
    //         x: String
    //         a: String
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       query: `
    //         fragment FragI on I {
    //           x
    //           ... on T {
    //             a
    //           }
    //         }
    //
    //         {
    //           t {
    //             ...FragI
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           t {
    //             x
    //             a
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn handles_fragment_matching_at_the_top_level_of_another_fragment() {
    //test('handles fragment matching at the top level of another fragment', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t: T
    //       }
    //
    //       type T {
    //         a: String
    //         u: U
    //       }
    //
    //       type U {
    //         x: String
    //         y: String
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       query: `
    //         fragment Frag1 on T {
    //           a
    //         }
    //
    //         fragment Frag2 on T {
    //           u {
    //             x
    //             y
    //           }
    //           ...Frag1
    //         }
    //
    //         fragment Frag3 on Query {
    //           t {
    //             ...Frag2
    //           }
    //         }
    //
    //         {
    //           ...Frag3
    //         }
    //       `,
    //       expanded: `
    //         {
    //           t {
    //             u {
    //               x
    //               y
    //             }
    //             a
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn handles_fragments_used_in_context_where_they_get_trimmed() {
    //test('handles fragments used in a context where they get trimmed', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t1: T1
    //       }
    //
    //       interface I {
    //         x: Int
    //       }
    //
    //       type T1 implements I {
    //         x: Int
    //         y: Int
    //       }
    //
    //       type T2 implements I {
    //         x: Int
    //         z: Int
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       query: `
    //         fragment FragOnI on I {
    //           ... on T1 {
    //             y
    //           }
    //           ... on T2 {
    //             z
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             ...FragOnI
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           t1 {
    //             y
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn handles_fragments_used_in_the_context_of_non_intersecting_abstract_types() {
    //test('handles fragments used in the context of non-intersecting abstract types', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         i2: I2
    //       }
    //
    //       interface I1 {
    //         x: Int
    //       }
    //
    //       interface I2 {
    //         y: Int
    //       }
    //
    //       interface I3 {
    //         z: Int
    //       }
    //
    //       type T1 implements I1 & I2 {
    //         x: Int
    //         y: Int
    //       }
    //
    //       type T2 implements I1 & I3 {
    //         x: Int
    //         z: Int
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       query: `
    //         fragment FragOnI1 on I1 {
    //           ... on I2 {
    //             y
    //           }
    //           ... on I3 {
    //             z
    //           }
    //         }
    //
    //         {
    //           i2 {
    //             ...FragOnI1
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           i2 {
    //             ... on I1 {
    //               ... on I2 {
    //                 y
    //               }
    //               ... on I3 {
    //                 z
    //               }
    //             }
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn handles_fragments_on_union_in_context_with_limited_intersection() {
    //test('handles fragments on union in context with limited intersection', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t1: T1
    //       }
    //
    //       union U = T1 | T2
    //
    //       type T1 {
    //         x: Int
    //       }
    //
    //       type T2 {
    //         y: Int
    //       }
    //     `);
    //
    //     testFragmentsRoundtrip({
    //       schema,
    //       query: `
    //         fragment OnU on U {
    //           ... on T1 {
    //             x
    //           }
    //           ... on T2 {
    //             y
    //           }
    //         }
    //
    //         {
    //           t1 {
    //             ...OnU
    //           }
    //         }
    //       `,
    //       expanded: `
    //         {
    //           t1 {
    //             x
    //           }
    //         }
    //       `,
    //     });
    //   });
}

#[test]
fn off_by_1_error() {
    //test('off by 1 error', () => {
    //     const schema = buildSchema(`#graphql
    //       type Query {
    //         t: T
    //       }
    //       type T {
    //         id: String!
    //         a: A
    //         v: V
    //       }
    //       type A {
    //         id: String!
    //       }
    //       type V {
    //         t: T!
    //       }
    //     `);
    //
    //     const operation = parseOperation(schema, `
    //       {
    //         t {
    //           ...TFrag
    //           v {
    //             t {
    //               id
    //               a {
    //                 __typename
    //                 id
    //               }
    //             }
    //           }
    //         }
    //       }
    //
    //       fragment TFrag on T {
    //         __typename
    //         id
    //       }
    //     `);
    //
    //     const withoutFragments = operation.expandAllFragments();
    //     expect(withoutFragments.toString()).toMatchString(`
    //       {
    //         t {
    //           __typename
    //           id
    //           v {
    //             t {
    //               id
    //               a {
    //                 __typename
    //                 id
    //               }
    //             }
    //           }
    //         }
    //       }
    //     `);
    //
    //     const optimized = withoutFragments.optimize(operation.fragments!);
    //     expect(optimized.toString()).toMatchString(`
    //       fragment TFrag on T {
    //         __typename
    //         id
    //       }
    //
    //       {
    //         t {
    //           ...TFrag
    //           v {
    //             t {
    //               ...TFrag
    //               a {
    //                 __typename
    //                 id
    //               }
    //             }
    //           }
    //         }
    //       }
    //     `);
    //   });
}

#[test]
fn removes_all_unused_fragments() {
    //test('does not leave unused fragments', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t1: T1
    //       }
    //
    //       union U1 = T1 | T2 | T3
    //       union U2 =      T2 | T3
    //
    //       type T1 {
    //         x: Int
    //       }
    //
    //       type T2 {
    //         y: Int
    //       }
    //
    //       type T3 {
    //         z: Int
    //       }
    //     `);
    //     const gqlSchema = schema.toGraphQLJSSchema();
    //
    //     const operation = parseOperation(schema, `
    //       query {
    //         t1 {
    //           ...Outer
    //         }
    //       }
    //
    //       fragment Outer on U1 {
    //         ... on T1 {
    //           x
    //         }
    //         ... on T2 {
    //           ... Inner
    //         }
    //         ... on T3 {
    //           ... Inner
    //         }
    //       }
    //
    //       fragment Inner on U2 {
    //         ... on T2 {
    //           y
    //         }
    //       }
    //     `);
    //     expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //     const withoutFragments = operation.expandAllFragments();
    //     expect(withoutFragments.toString()).toMatchString(`
    //       {
    //         t1 {
    //           x
    //         }
    //       }
    //     `);
    //
    //     // This is a bit of contrived example, but the reusing code will be able
    //     // to figure out that the `Outer` fragment can be reused and will initially
    //     // do so, but it's only use once, so it will expand it, which yields:
    //     // {
    //     //   t1 {
    //     //     ... on T1 {
    //     //       x
    //     //     }
    //     //     ... on T2 {
    //     //       ... Inner
    //     //     }
    //     //     ... on T3 {
    //     //       ... Inner
    //     //     }
    //     //   }
    //     // }
    //     // and so `Inner` will not be expanded (it's used twice). Except that
    //     // the `normalize` code is apply then and will _remove_ both instances
    //     // of `.... Inner`. Which is ok, but we must make sure the fragment
    //     // itself is removed since it is not used now, which this test ensures.
    //     const optimized = withoutFragments.optimize(operation.fragments!, 2);
    //     expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //     expect(optimized.toString()).toMatchString(`
    //       {
    //         t1 {
    //           x
    //         }
    //       }
    //     `);
    //   });
}

#[test]
fn removes_fragments_only_used_by_unused_fragments() {
    //test('does not leave fragments only used by unused fragments', () => {
    //     // Similar to the previous test, but we artificially add a
    //     // fragment that is only used by the fragment that is finally
    //     // unused.
    //
    //     const schema = parseSchema(`
    //       type Query {
    //         t1: T1
    //       }
    //
    //       union U1 = T1 | T2 | T3
    //       union U2 =      T2 | T3
    //
    //       type T1 {
    //         x: Int
    //       }
    //
    //       type T2 {
    //         y1: Y
    //         y2: Y
    //       }
    //
    //       type T3 {
    //         z: Int
    //       }
    //
    //       type Y {
    //         v: Int
    //       }
    //     `);
    //     const gqlSchema = schema.toGraphQLJSSchema();
    //
    //     const operation = parseOperation(schema, `
    //       query {
    //         t1 {
    //           ...Outer
    //         }
    //       }
    //
    //       fragment Outer on U1 {
    //         ... on T1 {
    //           x
    //         }
    //         ... on T2 {
    //           ... Inner
    //         }
    //         ... on T3 {
    //           ... Inner
    //         }
    //       }
    //
    //       fragment Inner on U2 {
    //         ... on T2 {
    //           y1 {
    //             ...WillBeUnused
    //           }
    //           y2 {
    //             ...WillBeUnused
    //           }
    //         }
    //       }
    //
    //       fragment WillBeUnused on Y {
    //         v
    //       }
    //     `);
    //     expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //     const withoutFragments = operation.expandAllFragments();
    //     expect(withoutFragments.toString()).toMatchString(`
    //       {
    //         t1 {
    //           x
    //         }
    //       }
    //     `);
    //
    //     const optimized = withoutFragments.optimize(operation.fragments!, 2);
    //     expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //     expect(optimized.toString()).toMatchString(`
    //       {
    //         t1 {
    //           x
    //         }
    //       }
    //     `);
    //   });
}

#[test]
fn keeps_fragments_used_by_other_fragments() {
    // test('keeps fragments only used by other fragments (if they are used enough times)', () => {
    //     const schema = parseSchema(`
    //       type Query {
    //         t1: T
    //         t2: T
    //       }
    //
    //       type T {
    //         a1: Int
    //         a2: Int
    //         b1: B
    //         b2: B
    //       }
    //
    //       type B {
    //         x: Int
    //         y: Int
    //       }
    //     `);
    //     const gqlSchema = schema.toGraphQLJSSchema();
    //
    //     const operation = parseOperation(schema, `
    //       query {
    //         t1 {
    //           ...TFields
    //         }
    //         t2 {
    //           ...TFields
    //         }
    //       }
    //
    //       fragment TFields on T {
    //         ...DirectFieldsOfT
    //         b1 {
    //           ...BFields
    //         }
    //         b2 {
    //           ...BFields
    //         }
    //       }
    //
    //       fragment DirectFieldsOfT on T {
    //         a1
    //         a2
    //       }
    //
    //       fragment BFields on B {
    //         x
    //         y
    //       }
    //     `);
    //     expect(validate(gqlSchema, parse(operation.toString()))).toStrictEqual([]);
    //
    //     const withoutFragments = operation.expandAllFragments();
    //     expect(withoutFragments.toString()).toMatchString(`
    //       {
    //         t1 {
    //           a1
    //           a2
    //           b1 {
    //             x
    //             y
    //           }
    //           b2 {
    //             x
    //             y
    //           }
    //         }
    //         t2 {
    //           a1
    //           a2
    //           b1 {
    //             x
    //             y
    //           }
    //           b2 {
    //             x
    //             y
    //           }
    //         }
    //       }
    //     `);
    //
    //     const optimized = withoutFragments.optimize(operation.fragments!, 2);
    //     expect(validate(gqlSchema, parse(optimized.toString()))).toStrictEqual([]);
    //
    //     // The `DirectFieldsOfT` fragments should not be kept as it is used only once within `TFields`,
    //     // but the `BFields` one should be kept.
    //     expect(optimized.toString()).toMatchString(`
    //       fragment BFields on B {
    //         x
    //         y
    //       }
    //
    //       fragment TFields on T {
    //         a1
    //         a2
    //         b1 {
    //           ...BFields
    //         }
    //         b2 {
    //           ...BFields
    //         }
    //       }
    //
    //       {
    //         t1 {
    //           ...TFields
    //         }
    //         t2 {
    //           ...TFields
    //         }
    //       }
    //     `);
    //   });
}

///
/// applied directives
///

#[test]
fn reuse_fragments_with_same_directive_on_the_fragment() {
    // test('reuse fragments with directives on the fragment, but only when there is those directives', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: T
    //           t2: T
    //           t3: T
    //         }
    //
    //         type T {
    //           a: Int
    //           b: Int
    //           c: Int
    //           d: Int
    //         }
    //       `);
    //
    //       testFragmentsRoundtrip({
    //         schema,
    //         query: `
    //           fragment DirectiveOnDef on T @include(if: $cond1) {
    //             a
    //           }
    //
    //           query myQuery($cond1: Boolean!, $cond2: Boolean!) {
    //             t1 {
    //               ...DirectiveOnDef
    //             }
    //             t2 {
    //               ... on T @include(if: $cond2) {
    //                 a
    //               }
    //             }
    //             t3 {
    //               ...DirectiveOnDef @include(if: $cond2)
    //             }
    //           }
    //         `,
    //         expanded: `
    //           query myQuery($cond1: Boolean!, $cond2: Boolean!) {
    //             t1 {
    //               ... on T @include(if: $cond1) {
    //                 a
    //               }
    //             }
    //             t2 {
    //               ... on T @include(if: $cond2) {
    //                 a
    //               }
    //             }
    //             t3 {
    //               ... on T @include(if: $cond1) @include(if: $cond2) {
    //                 a
    //               }
    //             }
    //           }
    //         `,
    //       });
    //     });
}

#[test]
fn reuse_fragments_with_same_directive_in_the_fragment_selection() {
    //test('reuse fragments with directives in the fragment selection, but only when there is those directives', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: T
    //           t2: T
    //           t3: T
    //         }
    //
    //         type T {
    //           a: Int
    //           b: Int
    //           c: Int
    //           d: Int
    //         }
    //       `);
    //
    //       testFragmentsRoundtrip({
    //         schema,
    //         query: `
    //           fragment DirectiveInDef on T {
    //             a @include(if: $cond1)
    //           }
    //
    //           query myQuery($cond1: Boolean!, $cond2: Boolean!) {
    //             t1 {
    //               a
    //             }
    //             t2 {
    //               ...DirectiveInDef
    //             }
    //             t3 {
    //               a @include(if: $cond2)
    //             }
    //           }
    //         `,
    //         expanded: `
    //           query myQuery($cond1: Boolean!, $cond2: Boolean!) {
    //             t1 {
    //               a
    //             }
    //             t2 {
    //               a @include(if: $cond1)
    //             }
    //             t3 {
    //               a @include(if: $cond2)
    //             }
    //           }
    //         `,
    //       });
    //     });
}

#[test]
fn reuse_fragments_with_directives_on_inline_fragments() {
    //test('reuse fragments with directives on spread, but only when there is those directives', () => {
    //       const schema = parseSchema(`
    //         type Query {
    //           t1: T
    //           t2: T
    //           t3: T
    //         }
    //
    //         type T {
    //           a: Int
    //           b: Int
    //           c: Int
    //           d: Int
    //         }
    //       `);
    //
    //       testFragmentsRoundtrip({
    //         schema,
    //         query: `
    //           fragment NoDirectiveDef on T {
    //             a
    //           }
    //
    //           query myQuery($cond1: Boolean!) {
    //             t1 {
    //               ...NoDirectiveDef
    //             }
    //             t2 {
    //               ...NoDirectiveDef @include(if: $cond1)
    //             }
    //           }
    //         `,
    //         expanded: `
    //           query myQuery($cond1: Boolean!) {
    //             t1 {
    //               a
    //             }
    //             t2 {
    //               ... on T @include(if: $cond1) {
    //                 a
    //               }
    //             }
    //           }
    //         `,
    //       });
    //     });
}

///
/// empty branches removal
///

#[test]
fn operation_not_modified_if_no_empty_branches() {
    //  it.each([
    //     '{ t { a } }',
    //     '{ t { a b } }',
    //     '{ t { a c { x y } } }',
    //   ])('is identity if there is no empty branch', (op) => {
    //     expect(withoutEmptyBranches(op)).toBe(op);
    //   });
}

#[test]
fn removes_simple_empty_branches() {
    //it('removes simple empty branches', () => {
    //     expect(withoutEmptyBranches(
    //       astSSet(
    //         astField('t', astSSet(
    //           astField('a'),
    //           astField('c', astSSet()),
    //         ))
    //       )
    //     )).toBe('{ t { a } }');
    //
    //     expect(withoutEmptyBranches(
    //       astSSet(
    //         astField('t', astSSet(
    //           astField('c', astSSet()),
    //           astField('a'),
    //         ))
    //       )
    //     )).toBe('{ t { a } }');
    //
    //     expect(withoutEmptyBranches(
    //       astSSet(
    //         astField('t', astSSet())
    //       )
    //     )).toBeUndefined();
    //   });
}

#[test]
fn removes_cascading_empty_branches() {
    //it('removes cascading empty branches', () => {
    //     expect(withoutEmptyBranches(
    //       astSSet(
    //         astField('t', astSSet(
    //           astField('c', astSSet()),
    //         ))
    //       )
    //     )).toBeUndefined();
    //
    //     expect(withoutEmptyBranches(
    //       astSSet(
    //         astField('u'),
    //         astField('t', astSSet(
    //           astField('c', astSSet()),
    //         ))
    //       )
    //     )).toBe('{ u }');
    //
    //     expect(withoutEmptyBranches(
    //       astSSet(
    //         astField('t', astSSet(
    //           astField('c', astSSet()),
    //         )),
    //         astField('u'),
    //       )
    //     )).toBe('{ u }');
    //   });
}
