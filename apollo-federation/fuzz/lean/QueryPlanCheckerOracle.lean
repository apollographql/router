import Apollo.Implementations.QueryPlanChecker

/-! Persistent native differential oracle for the Rust `query_plan_check` port.

This adapter holds a second, independent copy of the fixture and byte grammar defined in
`src/plan_model.rs`. Nothing is serialized between the two sides: the Rust harness sends the
raw bytes and each side decodes them with its own implementation of the same grammar. The
`schema` request exists so the runner can prove the two copies still agree before comparing
any verdict.

Protocol, one line in and one line out:

* `schema`          -> `ok=<digest>`
* `check <hex>`     -> `ok=<checkQueryPlan>`
* `quit`            -> exits

Diagnostics, for tracing a disagreement rather than only detecting one:

* `halves <hex>`     -> `ok=<complete>,<sound>`, the two halves of `checkQueryPlan` apart
* `fetchcheck <hex>` -> `ok=<with condition>,<without>`, the soundness of the entity fetch alone
* `guardsplit <hex>` -> completeness with each side's guard removed in turn
* `fetched <hex>`    -> the left operand of the completeness test, as this side builds it
* `operands <hex>`   -> both operands of the completeness test, separated by `|||`
* `requirement <hex>`-> both operands of the soundness inclusion test, separated by `|||`
* `render <hex>`     -> the decoded operation and plan, as this side builds them
-/

namespace QueryPlanCheckerOracle

open GraphQL
open GraphQL.Federation
open GraphQL.QueryInclusion

-----------------------------------------------------------------------------------------
-- The fixture (mirrors `plan_fixture::SUPERGRAPH_SDL`)
--
-- Wrapper types are dropped: the checker only ever asks for a field's named type. `T2.f` is
-- `Int!` and `T3.f` is `Int` in the supergraph, which is why the Rust side splits the entity
-- input fetches; neither side's checker sees the difference.
-----------------------------------------------------------------------------------------

/-- `__typename` is declared explicitly on every composite type here.
graphql-lean's `Schema` does not model introspection, so a schema handed to `includesBool` has to
declare it or every operation selecting it is invalid against that schema -- and validity is the
premise both of its theorems are stated under. `docs/query-plan.md` states this requirement; the
Rust side needs no counterpart, apollo-compiler knowing `__typename` natively. -/
def typenameField : FieldDefinition :=
  { name := "__typename", outputType := .named "String" }

def entityFields : List FieldDefinition :=
  [
    { name := "id", outputType := .named "ID" },
    { name := "f", outputType := .named "Int" },
    { name := "g", outputType := .named "Int" },
    typenameField
  ]

def schema : Schema :=
  {
    queryType := "Query"
    types :=
      [
        .object
          {
            name := "Query"
            fields := [{ name := "is", outputType := .list (.named "I") }, typenameField]
          },
        .interface { name := "I", fields := entityFields },
        .object { name := "T1", fields := entityFields, interfaces := ["I"] },
        .object { name := "T2", fields := entityFields, interfaces := ["I"] },
        .object { name := "T3", fields := entityFields, interfaces := ["I"] }
      ]
  }

def objectTypes : List Name := ["T1", "T2", "T3"]

/-- The types an entity fetch may ask about: those carrying a resolvable `@key` in Subgraph2. -/
def entityTypes : List Name := ["T2", "T3"]

def keyFields : Name := "id"

def requiresFields : Name := "f"

def variables : List Name := ["v0", "v1"]

/-- A field set of one leaf field, which is all `@key` and `@requires` are here. -/
def fieldSetOf (name : Name) : FieldSet := [.field name [] []]

def subgraph2 : Subgraph :=
  {
    schema := schema
    keys := fun typeName =>
      if entityTypes.contains typeName then [{ fields := fieldSetOf keyFields }] else []
    requires := fun typeName fieldName =>
      if entityTypes.contains typeName && fieldName == "g" then
        Option.some (fieldSetOf requiresFields)
      else
        Option.none
  }

def subgraph1 : Subgraph :=
  {
    schema := schema
    keys := fun typeName =>
      if entityTypes.contains typeName then [{ fields := fieldSetOf keyFields }] else []
  }

def subgraphs : Subgraphs := fun name =>
  if name == "Subgraph1" then Option.some subgraph1
  else if name == "Subgraph2" then Option.some subgraph2
  else Option.none

-----------------------------------------------------------------------------------------
-- The grammar (mirrors `plan_model`)
-----------------------------------------------------------------------------------------

def selectionSlots : Nat := 8

def maxSelections : Nat := 3

def leaf (name : Name) : Selection := .field name name [] [] []

def under (typeName : Name) (directives : List DirectiveApplication)
    (selections : List Selection)
    : Selection :=
  .inlineFragment (Option.some typeName) directives selections

/-- One selection under `is`, by slot. `g` never appears unguarded by a type condition; see
`plan_model::selection_source`. -/
def selectionSource (slot : Nat) (directives : List DirectiveApplication) : Selection :=
  match slot % selectionSlots with
  | 0 => leaf "id"
  | 1 => leaf "f"
  | 2 => leaf "__typename"
  | 3 => under "T1" [] [leaf "g"]
  | 4 => under "T2" directives [leaf "g"]
  | 5 => under "T3" directives [leaf "g"]
  | 6 => under "T2" [] [leaf "f"]
  | _ => under "T3" [] [leaf "id"]

/-- Which entity type a slot needs an entity fetch for, if any. -/
def slotNeedsEntityFetch (slot : Nat) : Option Name :=
  match slot % selectionSlots with
  | 4 => Option.some "T2"
  | 5 => Option.some "T3"
  | _ => Option.none

inductive Perturbation where
  | unchanged
  | dropRequiredField
  | dropRequiresEntry
  | dropEntityCase
  | wrongKeyField
  | flattenWrongKey
  | dropKeyField
  | guardEntityFetch
deriving Repr, DecidableEq

def perturbations : List Perturbation :=
  [
    .unchanged,
    .dropRequiredField,
    .dropRequiresEntry,
    .dropEntityCase,
    .wrongKeyField,
    .flattenWrongKey,
    .dropKeyField,
    .guardEntityFetch
  ]

structure Case where
  slots : List Nat
  guard : Option Name
  perturbation : Perturbation

/-- Adds a slot unless it is already there, keeping first-use order. -/
def pushSlot (slots : List Nat) (slot : Nat) : List Nat :=
  if slots.contains slot then slots else slots ++ [slot]

def decodeSlots : Nat -> List Nat -> List Nat -> List Nat × List Nat
  | 0, slots, rest => (slots, rest)
  | _count + 1, slots, [] => (slots, [])
  | count + 1, slots, byte :: rest =>
      decodeSlots count (pushSlot slots (byte % selectionSlots)) rest

/-- Decodes one case. `Option.none` when the bytes run out, which is how short inputs are discarded
rather than silently padded. -/
def decodeCase : List Nat -> Option Case
  | countByte :: rest =>
      let count := 1 + countByte % maxSelections
      let (slots, rest) := decodeSlots count [] rest
      match rest with
      | guardByte :: perturbationByte :: _ =>
          let guard :=
            match guardByte % 3 with
            | 0 => Option.none
            | other => variables[(other - 1) % variables.length]?
          let perturbation :=
            (perturbations[perturbationByte % perturbations.length]?).getD .unchanged
          Option.some { slots, guard, perturbation }
      | _ => Option.none
  | [] => Option.none

namespace Case

/-- The entity types this operation needs an entity fetch for, in fixture order. -/
def entityCasesOf (case : Case) : List Name :=
  entityTypes.filter fun entityType =>
    case.slots.any fun slot => slotNeedsEntityFetch slot == Option.some entityType

def guardDirectives (case : Case) : List DirectiveApplication :=
  match case.guard with
  | Option.none => []
  | Option.some variableName => [.include (.variable variableName)]

/-- The client operation. -/
def operation (case : Case) : Operation :=
  {
    selectionSet :=
      [.field "is" "is" [] []
        (case.slots.map fun slot => selectionSource slot (guardDirectives case))]
  }

end Case

-----------------------------------------------------------------------------------------
-- The miniature planner (mirrors `plan_model::build_plan`)
-----------------------------------------------------------------------------------------

def fetchOf (subgraphName : Name) (requiresItems : List RequiresSelection)
    (selectionSet : SelectionSet)
    : FetchNode :=
  {
    subgraphName := subgraphName
    requires := requiresItems
    operationDocument := { selectionSet := selectionSet }
  }

/-- What the base fetch selects under `is`: everything Subgraph1 resolves without an entity
fetch. -/
def baseFetchBody (case : Case) : SelectionSet :=
  leaf "__typename"
  :: (case.slots.filter fun slot => (slotNeedsEntityFetch slot).isNone).map
      fun slot => selectionSource slot []

/-- What Subgraph1 is asked for to make one entity type fetchable. -/
def entityInputBody (case : Case) (entityType : Name) : SelectionSet :=
  let inner :=
    [leaf "__typename"]
    ++ (if case.perturbation == .dropKeyField then [] else [leaf keyFields])
    ++ (if case.perturbation == .dropRequiredField then [] else [leaf requiresFields])
  [leaf "__typename", under entityType [] inner]

def requiresEntries (case : Case) : List RequiresSelection :=
  let keyField := if case.perturbation == .wrongKeyField then requiresFields else keyFields
  let entries :=
    (Case.entityCasesOf case).map fun entityType =>
      RequiresSelection.inlineFragment (Option.some entityType)
        [.field "__typename" [] [], .field keyField [] [], .field requiresFields [] []]
  if case.perturbation == .dropRequiresEntry && entries.length > 1 then
    entries.dropLast
  else
    entries

def entityCaseTypes (case : Case) : List Name :=
  let cases := Case.entityCasesOf case
  if case.perturbation == .dropEntityCase && cases.length > 1 then cases.dropLast else cases

/-- An index consumes no response key, so dropping it would not be a perturbation; naming a key
the plan never fetched does mount nowhere. -/
def flattenPath (case : Case) : FetchDataPath :=
  let key := if case.perturbation == .flattenWrongKey then "g" else "is"
  [.key key Option.none, .anyIndex Option.none]

def buildPlan (case : Case) : QueryPlan :=
  let inputs :=
    PlanNode.fetch (fetchOf "Subgraph1" [] [.field "is" "is" [] [] (baseFetchBody case)])
    :: (Case.entityCasesOf case).map
        fun entityType =>
          PlanNode.fetch
            (fetchOf "Subgraph1" []
              [.field "is" "is" [] [] (entityInputBody case entityType)])
  let cases := entityCaseTypes case
  if cases.isEmpty then
    { node := Option.some (.plan (.parallel inputs)) }
  else
    let entityFetch :=
      fetchOf "Subgraph2" (requiresEntries case)
        [.field entitiesFieldName entitiesFieldName [] []
          (cases.map fun entityType => under entityType [] [leaf "g"])]
    let mounted := PlanNode.flatten (flattenPath case) (.fetch entityFetch)
    let condition :=
      if case.perturbation == .guardEntityFetch then
        variables[variables.length - 1]?
      else
        case.guard
    let guarded :=
      match condition with
      | Option.none => mounted
      | Option.some variableName => PlanNode.condition variableName (Option.some mounted) Option.none
    { node := Option.some (.plan (.sequence [.parallel inputs, guarded])) }

-----------------------------------------------------------------------------------------
-- The digest
-----------------------------------------------------------------------------------------

def space : String := " "

mutual

  def renderDirectives : List DirectiveApplication -> String
    | [] => ""
    | .include (.variable name) :: rest => "@include(if: $" ++ toString name ++ ")" ++ renderDirectives rest
    | .skip (.variable name) :: rest => "@skip(if: $" ++ toString name ++ ")" ++ renderDirectives rest
    | _ :: rest => "@?" ++ renderDirectives rest

  def renderSelection : Selection -> String
    | .field responseName _fieldName _arguments directives [] =>
        toString responseName ++ renderDirectives directives
    | .field responseName _fieldName _arguments directives selections =>
        toString responseName ++ renderDirectives directives
          ++ " { " ++ renderSelections selections ++ " }"
    | .inlineFragment typeCondition directives selections =>
        let condition :=
          match typeCondition with
          | Option.none => ""
          | Option.some name => "on " ++ toString name ++ " "
        "... " ++ condition ++ renderDirectives directives
          ++ "{ " ++ renderSelections selections ++ " }"

  def renderSelections : List Selection -> String
    | [] => ""
    | [selection] => renderSelection selection
    | selection :: rest => renderSelection selection ++ space ++ renderSelections rest

end

/-- The types the digest reports field declarations for, in a fixed order. -/
def digestTypes : List Name := ["Query", "I", "T1", "T2", "T3"]

/-- One type's fields as `name:NamedType`, sorted, so the two copies of the fixture can be
compared on what each type actually declares. Wrappers are dropped: the Rust side reads the
supergraph schema, where `is` is `[I!]!` and `T2.f` is `Int!`, and neither checker looks past the
named type. -/
def declaredFields : TypeDefinition -> List FieldDefinition
  | .object objectType => objectType.fields
  | .interface interfaceType => interfaceType.fields
  | _typeDefinition => []

def renderTypeFields (typeName : Name) : String :=
  let fields :=
    match schema.types.find? (fun ty => ty.name == typeName) with
    | Option.none => []
    | Option.some ty =>
        (declaredFields ty).map fun (field : FieldDefinition) =>
          toString field.name ++ ":" ++ toString field.outputType.namedType
  typeName ++ "{ " ++ String.intercalate space (fields.mergeSort (fun a b => decide (a <= b)))
    ++ " }"

def renderNames (names : List Name) : String :=
  String.intercalate space (names.map toString)

def schemaDigest : String :=
  let slots := (List.range selectionSlots).map fun slot =>
    renderSelection (selectionSource slot [])
  "objects: " ++ renderNames objectTypes
  ++ " | entities: " ++ renderNames entityTypes
  ++ " | key: " ++ toString keyFields
  ++ " | requires: " ++ toString requiresFields
  ++ " | variables: " ++ renderNames variables
  ++ " | selections: " ++ String.intercalate space slots
  ++ " | fields: " ++ String.intercalate space (digestTypes.map renderTypeFields)

-----------------------------------------------------------------------------------------
-- The line protocol
-----------------------------------------------------------------------------------------

def hexDigit (ch : Char) : Option Nat :=
  if ch.isDigit then Option.some (ch.toNat - '0'.toNat)
  else if 'a' <= ch && ch <= 'f' then Option.some (ch.toNat - 'a'.toNat + 10)
  else if 'A' <= ch && ch <= 'F' then Option.some (ch.toNat - 'A'.toNat + 10)
  else Option.none

def parseHex (text : String) : List Nat :=
  let rec go : List Char -> List Nat
    | high :: low :: rest =>
        match hexDigit high, hexDigit low with
        | Option.some h, Option.some l => (h * 16 + l) :: go rest
        | _, _ => []
    | _ => []
  go text.toList

def flag (value : Bool) : String := if value then "1" else "0"

def stripTrailing (text : String) : String :=
  String.ofList (text.toList.reverse.dropWhile (fun ch => ch == '\n' || ch == '\r')).reverse

def splitCommand (text : String) : String × String :=
  let chars := text.toList
  let command := chars.takeWhile (fun ch => ch != ' ')
  let rest := (chars.dropWhile (fun ch => ch != ' ')).dropWhile (fun ch => ch == ' ')
  (String.ofList command, String.ofList rest)

partial def loop : IO Unit := do
  let stdin <- IO.getStdin
  let line <- stdin.getLine
  if line.isEmpty then
    return
  let (command, argument) := splitCommand (stripTrailing line)
  if command == "quit" then
    return
  else if command == "schema" then
    IO.println s!"ok={schemaDigest}"
    (<- IO.getStdout).flush
    loop
  else if command == "render" then
    -- Diagnostic: the decoded operation and plan as this side built them, so a divergence can be
    -- traced to the grammar rather than to the checkers.
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some testCase =>
        let rendered := s!"{repr testCase.operation} ||| {repr (buildPlan testCase)}"
        IO.println s!"ok={rendered.replace "\n" " "}"
    (<- IO.getStdout).flush
    loop
  else if command == "fetchcheck" then
    -- Diagnostic: the soundness half of one entity fetch, taken apart. Reports what `available`
    -- the walk reaches it with, then the two conjuncts of `checkRequirementMatchesCase` -- the
    -- syntactic match and the inclusion test -- with the fetch's own condition and without it.
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some testCase =>
        let operation := testCase.operation
        let inputs :=
          PlanNode.parallel
            (PlanNode.fetch
              (fetchOf "Subgraph1" [] [.field "is" "is" [] [] (baseFetchBody testCase)])
            :: (Case.entityCasesOf testCase).map
                fun entityType =>
                  PlanNode.fetch
                    (fetchOf "Subgraph1" []
                      [.field "is" "is" [] [] (entityInputBody testCase entityType)]))
        let available := (inputs.check schema subgraphs operation [] [] []).1
        let cases := entityCaseTypes testCase
        let entityFetch :=
          fetchOf "Subgraph2" (requiresEntries testCase)
            [.field entitiesFieldName entitiesFieldName [] []
              (cases.map fun entityType => under entityType [] [leaf "g"])]
        let path := flattenPath testCase
        let guarded : BooleanCondition :=
          match testCase.guard with
          | Option.none => []
          | Option.some variableName => [.positive variableName]
        let withCondition :=
          checkFetch schema subgraphs operation path guarded available entityFetch
        let withoutCondition :=
          checkFetch schema subgraphs operation path [] available entityFetch
        IO.println s!"ok={flag withCondition},{flag withoutCondition}"
    (<- IO.getStdout).flush
    loop
  else if command == "guardsplit" then
    -- Diagnostic: the completeness half with each side's guard removed in turn, which says
    -- whether the plan's condition node or the operation's own directive is what trips it.
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some testCase =>
        let unguardedCase := { testCase with guard := Option.none }
        let complete (operation : Operation) (plan : QueryPlan) : Bool :=
          includesBool schema
            { operation with selectionSet := (plan.check schema subgraphs operation).1 }
            operation
        let guardedPlan := buildPlan testCase
        let unguardedPlan := buildPlan unguardedCase
        IO.println
          s!"ok={flag (complete testCase.operation guardedPlan)},\
             {flag (complete unguardedCase.operation guardedPlan)},\
             {flag (complete testCase.operation unguardedPlan)},\
             {flag (complete unguardedCase.operation unguardedPlan)}"
    (<- IO.getStdout).flush
    loop
  else if command == "requirement" then
    -- Diagnostic: the two operands of the *soundness* inclusion test, for the entity fetch's
    -- first case and first key -- what it has already fetched, and what that key demands of it.
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some testCase =>
        let operation := testCase.operation
        let inputs :=
          PlanNode.parallel
            (PlanNode.fetch
              (fetchOf "Subgraph1" [] [.field "is" "is" [] [] (baseFetchBody testCase)])
            :: (Case.entityCasesOf testCase).map
                fun entityType =>
                  PlanNode.fetch
                    (fetchOf "Subgraph1" []
                      [.field "is" "is" [] [] (entityInputBody testCase entityType)]))
        let available := (inputs.check schema subgraphs operation [] [] []).1
        let guarded : BooleanCondition :=
          match testCase.guard with
          | Option.none => []
          | Option.some variableName => [.positive variableName]
        match (entityCaseTypes testCase).head?, requiresEntries testCase with
        | Option.some entityType, .inlineFragment (Option.some requireType) _ :: _ =>
            match (subgraph2.keys entityType).head? with
            | Option.none => IO.println "ok=nokey"
            | Option.some key =>
                let computed :=
                  entityFetchRequirement subgraph2 entityType requireType [leaf "g"] key
                let required :=
                  requiredAt schema (flattenPath testCase) guarded available computed
                IO.println s!"ok={renderSelections available} ||| {renderSelections required}"
        | _, _ => IO.println "ok=nocase"
    (<- IO.getStdout).flush
    loop
  else if command == "operands" then
    -- Diagnostic: both operands of the completeness test, as this side builds them, separated by
    -- `|||`. What a disagreement there is really about, with the plan machinery rendered away.
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some testCase =>
        let operation := testCase.operation
        let walked := (buildPlan testCase).check schema subgraphs operation
        IO.println s!"ok={renderSelections walked.1} ||| {renderSelections operation.selectionSet}"
    (<- IO.getStdout).flush
    loop
  else if command == "fetched" then
    -- Diagnostic: the left operand of the completeness test, as this side builds it.
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some testCase =>
        let operation := testCase.operation
        let walked := (buildPlan testCase).check schema subgraphs operation
        IO.println s!"ok={renderSelections walked.1}"
    (<- IO.getStdout).flush
    loop
  else if command == "halves" then
    -- Diagnostic: `checkQueryPlan` is one inclusion test and one walk verdict; reporting them
    -- apart says which half a disagreement is in.
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some testCase =>
        let operation := testCase.operation
        let plan := buildPlan testCase
        let walked := plan.check schema subgraphs operation
        let complete := includesBool schema { operation with selectionSet := walked.1 } operation
        IO.println s!"ok={flag complete},{flag walked.2}"
    (<- IO.getStdout).flush
    loop
  else if command == "check" then
    match decodeCase (parseHex argument) with
    | Option.none => IO.println "ok=skip"
    | Option.some case =>
        IO.println s!"ok={flag (checkQueryPlan schema subgraphs case.operation (buildPlan case))}"
    (<- IO.getStdout).flush
    loop
  else
    IO.println s!"error=unknown request: {command}"
    (<- IO.getStdout).flush
    loop

end QueryPlanCheckerOracle

def main : IO Unit := QueryPlanCheckerOracle.loop
