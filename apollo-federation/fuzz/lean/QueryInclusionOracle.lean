import GraphQL.Theories.QueryInclusion

/-! Persistent native differential oracle for the Rust `query_compare` port.

This adapter holds a second, independent copy of the schema and byte grammar defined in
`src/model.rs`. Nothing is serialized between the two sides: the Rust harness sends the raw
bytes and each side decodes them with its own implementation of the same grammar. The `schema`
request exists so the runner can prove the two copies still agree before comparing any verdict.

Protocol, one line in and one line out:

* `schema`          -> `ok=<digest>`
* `includes <hex>`  -> `ok=<fwd includes>,<fwd reference>,<bwd includes>,<bwd reference>`
* `quit`            -> exits
-/

namespace QueryInclusionOracle

open GraphQL
open GraphQL.QueryInclusion

-----------------------------------------------------------------------------------------
-- The fixed schema (mirrors `model::SCHEMA_SDL`)
--
-- Wrapper types are irrelevant to `includesBool`, which only ever asks for a field's named
-- type and whether that type is composite, so list and non-null wrappers are dropped here.
-- Argument *definitions* are likewise unused: operation arguments are compared syntactically.
-----------------------------------------------------------------------------------------

def leaf (name : Name) : FieldDefinition :=
  { name, outputType := .named "String" }

def commonFields (friendType : Name) : List FieldDefinition :=
  [
    leaf "name",
    leaf "id",
    leaf "tag",
    { name := "friend", outputType := .named friendType },
    { name := "friends", outputType := .list (.named "Animal") },
    { name := "pack", outputType := .named "Pack" }
  ]

def schema : Schema :=
  {
    queryType := "Query"
    types :=
      [
        .object { name := "Query", fields := [{ name := "animals", outputType := .list (.named "Animal") }] },
        .interface { name := "Animal", fields := commonFields "Animal" },
        .interface { name := "I", fields := commonFields "Animal" },
        .interface { name := "J", fields := commonFields "Animal" },
        .interface { name := "K", fields := commonFields "Animal" },
        .object
          {
            name := "Dog"
            fields := commonFields "Dog" ++ [leaf "bark"]
            interfaces := ["Animal", "I", "J", "K"]
          },
        .object
          {
            name := "Cat"
            fields := commonFields "Cat" ++ [leaf "purr"]
            interfaces := ["Animal", "I", "K"]
          },
        .object { name := "Fox", fields := commonFields "Animal", interfaces := ["Animal", "J"] },
        .union { name := "Pack", members := ["Dog", "Cat"] }
      ]
  }

def compositeTypes : List Name := ["Animal", "I", "J", "K", "Dog", "Cat", "Fox", "Pack"]

def fieldNames : List Name := ["name", "id", "tag", "friend", "friends", "pack", "bark", "purr"]

-- Slot 0 draws only from the leading universal leaves, which guarantees the fallback in
-- `decodeSelection` always finds a selectable slot.
def universalLeafFields : Nat := 3

def isUnion (typeName : Name) : Bool := typeName == "Pack"

def fieldTakesArgument (field : Name) : Bool := field == "tag"

def fieldDefinedOn (parentType field : Name) : Bool :=
  if field == "bark" then parentType == "Dog"
  else if field == "purr" then parentType == "Cat"
  else !isUnion parentType

-- Two covariant overrides, so a narrowed scope can reach two different child regions.
def fieldOutputType (parentType field : Name) : Option Name :=
  if field == "friend" then
    some (if parentType == "Dog" then "Dog" else if parentType == "Cat" then "Cat" else "Animal")
  else if field == "friends" then
    some "Animal"
  else if field == "pack" then
    some "Pack"
  else
    none

def insertSorted (name : Name) : List Name -> List Name
  | [] => [name]
  | candidate :: rest =>
      if name ≤ candidate then name :: candidate :: rest else candidate :: insertSorted name rest

def sortNames : List Name -> List Name
  | [] => []
  | name :: rest => insertSorted name (sortNames rest)

-- Sorted by name, matching the Rust side's canonical region order.
def possibleTypes (typeName : Name) : List Name :=
  sortNames (schema.getPossibleTypes typeName)

-- Unions are excluded: narrowing a union scope to another union would leave the grammar still
-- unable to emit a field.
def validTypeConditions (parentType : Name) : List Name :=
  let parentPossible := possibleTypes parentType
  (compositeTypes.filter fun candidate => !isUnion candidate).filter
    fun candidate => (possibleTypes candidate).any fun ty => parentPossible.contains ty

def variableDeclarations : Nat := 5

def renderVariableDeclaration (name : Name) (variant : Nat) : String :=
  match variant % variableDeclarations with
  | 0 => s!"${name}: Boolean!"
  | 1 => s!"${name}: Boolean! = true"
  | 2 => s!"${name}: Boolean! = false"
  | 3 => s!"${name}: Boolean = true"
  | _ => s!"${name}: Boolean = false"

-- `Int!` and `String` variables, referenced from argument position. A nullable variable needs a
-- non-null default to be usable at a non-null location, so there is no bare `Int`.
def intVariables : List Name := ["i0", "i1"]
def stringVariables : List Name := ["s0"]
def intDeclarations : Nat := 3
def stringDeclarations : Nat := 2

def renderIntDeclaration (name : Name) (variant : Nat) : String :=
  match variant % intDeclarations with
  | 0 => s!"${name}: Int!"
  | 1 => s!"${name}: Int! = 1"
  | _ => s!"${name}: Int = 1"

def renderStringDeclaration (name : Name) (variant : Nat) : String :=
  match variant % stringDeclarations with
  | 0 => s!"${name}: String"
  | _ => s!"${name}: String = \"a\""

def intVariableDefinition (name : Name) (variant : Nat) : VariableDefinition :=
  match variant % intDeclarations with
  | 0 => { name, typeRef := .nonNull (.named "Int") }
  | 1 => { name, typeRef := .nonNull (.named "Int"), defaultValue := some (.int 1) }
  | _ => { name, typeRef := .named "Int", defaultValue := some (.int 1) }

def stringVariableDefinition (name : Name) (variant : Nat) : VariableDefinition :=
  match variant % stringDeclarations with
  | 0 => { name, typeRef := .named "String" }
  | _ => { name, typeRef := .named "String", defaultValue := some (.string "a") }

def variableDefinition (name : Name) (variant : Nat) : VariableDefinition :=
  match variant % variableDeclarations with
  | 0 => { name, typeRef := .nonNull (.named "Boolean") }
  | 1 => { name, typeRef := .nonNull (.named "Boolean"), defaultValue := some (.boolean true) }
  | 2 => { name, typeRef := .nonNull (.named "Boolean"), defaultValue := some (.boolean false) }
  | 3 => { name, typeRef := .named "Boolean", defaultValue := some (.boolean true) }
  | _ => { name, typeRef := .named "Boolean", defaultValue := some (.boolean false) }

def schemaDigest : String :=
  let types :=
    compositeTypes.foldl
      (fun acc name =>
        acc
          ++ s!"{name}:{String.intercalate "," (possibleTypes name)}"
          ++ s!":{if isUnion name then 1 else 0};")
      ""
  let fields :=
    fieldNames.foldl
      (fun acc field =>
        let perParent :=
          ["Animal", "Dog", "Cat", "Fox"].foldl
            (fun inner parent =>
              inner
                ++ s!"{if fieldDefinedOn parent field then 1 else 0}"
                ++ s!"{(fieldOutputType parent field).getD "-"}")
            ""
        acc ++ s!"{field}/{if fieldTakesArgument field then 1 else 0}/" ++ perParent ++ ";")
      types
  let booleans :=
    (List.range variableDeclarations).foldl
      (fun acc variant => acc ++ s!"{renderVariableDeclaration "v" variant};") fields
  let ints :=
    (List.range intDeclarations).foldl
      (fun acc variant => acc ++ s!"{renderIntDeclaration "i" variant};") booleans
  (List.range stringDeclarations).foldl
    (fun acc variant => acc ++ s!"{renderStringDeclaration "s" variant};") ints

-----------------------------------------------------------------------------------------
-- The byte grammar (mirrors the decoder in `model.rs`)
-----------------------------------------------------------------------------------------

def nestingBudget : Nat := 5
def responseSlots : Nat := 4
def directiveChoices : Nat := 12

structure Cursor where
  bytes : Array Nat
  pos : Nat

-- Past the end every byte reads as zero, so decoding is total for every input.
def Cursor.next (c : Cursor) : Nat × Cursor :=
  ((c.bytes[c.pos]?).getD 0, { c with pos := c.pos + 1 })

-- Chosen to reach every arm the checker's value comparison distinguishes. `tags` is a list, whose
-- items compare order-sensitively; `meta` is an input object, whose fields compare
-- order-insensitively.
structure Arguments where
  n : Nat
  label : Option String
  flag : Option Bool
  tags : Option (List String)
  metaArg : Option (List (Name × InputValue))
  nVariable : Option Name
  labelVariable : Option Name
  kind : Option Name
  swapped : Bool

structure Slot where
  field : Name
  arguments : Arguments

def tagLists : List (List String) := [["a"], ["a", "b"], ["b", "a"]]

def metaObjects : List (List (Name × InputValue)) :=
  [
    [("count", .int 1)],
    [("count", .int 1), ("nested", .list [.int 1])],
    [("nested", .list [.int 1]), ("count", .int 1)]
  ]

-- Two bytes of argument choices, mirroring `decode_arguments` in `model.rs`.
def decodeArguments (first second : Nat) : Arguments :=
  {
    n := first % 2
    nVariable :=
      match first / 18 % 3 with
      | 0 => none
      | other => intVariables[other - 1]?
    labelVariable :=
      match second / 96 % 2 with
      | 0 => none
      | _ => stringVariables[0]?
    label :=
      match first / 2 % 3 with
      | 0 => none
      | 1 => some "a"
      | _ => some "b"
    flag :=
      match first / 6 % 3 with
      | 0 => none
      | 1 => some true
      | _ => some false
    tags :=
      match second % 4 with
      | 0 => none
      | other => tagLists[other - 1]?
    metaArg :=
      match second / 4 % 4 with
      | 0 => none
      | other => metaObjects[other - 1]?
    kind :=
      match second / 16 % 3 with
      | 0 => none
      | 1 => some "A"
      | _ => some "B"
    swapped := second / 48 % 2 == 1
  }

-- Only the shapes `metaObjects` actually uses; a general printer would need its own termination
-- proof for no benefit.
def describeMetaValue : InputValue -> String
  | .int value => toString value
  | .list values =>
      "[" ++ String.intercalate ","
        (values.map fun value => match value with | .int n => toString n | _ => "?") ++ "]"
  | _ => "?"

def describeArguments (a : Arguments) : String :=
  let optional : Option String -> String := fun value => value.getD "-"
  let metaDescription :=
    a.metaArg.map fun fields =>
      String.intercalate "," (fields.map fun (name, value) => s!"{name}:{describeMetaValue value}")
  s!"n{a.n}{optional a.nVariable}|l{optional a.label}{optional a.labelVariable}"
    ++ s!"|f{optional (a.flag.map fun f => if f then "true" else "false")}"
    ++ s!"|t{optional (a.tags.map fun tags => String.intercalate "+" tags)}"
    ++ s!"|m{optional metaDescription}|k{optional a.kind}|s{if a.swapped then 1 else 0}"

-- Every argument encoding the grammar can produce, described canonically. The argument encoding is
-- the most drift-prone half of the shared grammar, so the oracle reports it alongside the schema.
def argumentDigest : String :=
  (List.range 24).foldl
    (fun acc index => acc ++ s!"{describeArguments (decodeArguments (index * 7) (index * 11))};") ""

def decodeSlot (index : Nat) (c : Cursor) : Slot × Cursor :=
  let (fieldByte, c) := c.next
  let choices := if index == 0 then universalLeafFields else fieldNames.length
  let field := (fieldNames[fieldByte % choices]?).getD "name"
  let (first, c) := c.next
  let (second, c) := c.next
  ({ field, arguments := decodeArguments first second }, c)

def decodeSlots : Nat -> Nat -> Cursor -> List Slot × Cursor
  | _index, 0, c => ([], c)
  | index, n + 1, c =>
      let (slot, c) := decodeSlot index c
      let (rest, c) := decodeSlots (index + 1) n c
      (slot :: rest, c)

def directivesFor (choice : Nat) : List DirectiveApplication :=
  if choice == 1 then [.include (.variable "v0")]
  else if choice == 2 then [.skip (.variable "v0")]
  else if choice == 3 then [.include (.variable "v1")]
  else if choice == 4 then [.skip (.variable "v1")]
  else if choice == 5 then [.include (.variable "v2")]
  else if choice == 6 then [.skip (.variable "v2")]
  else if choice == 7 then [.include (.boolean true)]
  else if choice == 8 then [.skip (.boolean false)]
  else if choice == 9 then [.include (.variable "v0"), .skip (.variable "v1")]
  else if choice == 10 then [.include (.variable "v1"), .skip (.variable "v2")]
  else if choice == 11 then [.skip (.variable "v0"), .include (.variable "v2")]
  else []

def emptyArguments : Arguments :=
  {
    n := 0, label := none, flag := none, tags := none, metaArg := none, kind := none
    nVariable := none, labelVariable := none, swapped := false
  }

def slotAt (slots : List Slot) (index : Nat) : Slot :=
  (slots[index]?).getD { field := "name", arguments := emptyArguments }

-- Argument order is not semantically meaningful, so both sides must agree that a permutation
-- changes nothing; the decoder emits both orders.
def slotArguments (slot : Slot) : List Argument :=
  if !fieldTakesArgument slot.field then
    []
  else
    let a := slot.arguments
    let nValue : InputValue :=
      match a.nVariable with
      | some name => .variable name
      | none => .int (Int.ofNat a.n)
    let labelValue : String -> InputValue := fun label =>
      match a.labelVariable with
      | some name => .variable name
      | none => .string label
    let parts : List Argument :=
      [{ name := "n", value := nValue }]
        ++ (a.label.toList.map fun label => { name := "label", value := labelValue label })
        ++ (a.flag.toList.map fun flag => { name := "flag", value := .boolean flag })
        ++ (a.tags.toList.map
              fun tags => { name := "tags", value := .list (tags.map InputValue.string) })
        ++ (a.metaArg.toList.map fun fields => { name := "meta", value := .object fields })
        ++ (a.kind.toList.map fun kind => { name := "kind", value := .enum kind })
    if a.swapped then parts.reverse else parts

-- A field must be declared on the parent, a composite field needs room for its selection set, and
-- a union-returning field needs two units: one for its own set and one for the inline fragment
-- that set is required to consist of.
def slotSelectable (slot : Slot) (parentType : Name) (budget : Nat) : Bool :=
  if !fieldDefinedOn parentType slot.field then
    false
  else
    match fieldOutputType parentType slot.field with
    | none => true
    | some child => if isUnion child then budget ≥ 2 else budget ≥ 1

def firstSelectableSlot (slots : List Slot) (parentType : Name) (budget fallback : Nat) : Nat :=
  let rec go : Nat -> List Slot -> Option Nat
    | _, [] => none
    | index, slot :: rest =>
        if slotSelectable slot parentType budget then some index else go (index + 1) rest
  (go 0 slots).getD fallback

def mkField (slots : List Slot) (slotIndex : Nat)
    (directives : List DirectiveApplication) (children : List Selection)
    : Selection :=
  let slot := slotAt slots slotIndex
  .field s!"r{slotIndex}" slot.field (slotArguments slot) directives children

mutual
  def decodeSelections (slots : List Slot) (parentType : Name) (budget : Nat)
      : Nat -> Cursor -> List Selection × Cursor
    | 0, c => ([], c)
    | n + 1, c =>
        let (selection, c) := decodeSelection slots parentType budget c
        let (rest, c) := decodeSelections slots parentType budget n c
        (selection :: rest, c)
  termination_by n => (budget, 1, n)

  def decodeSelection (slots : List Slot) (parentType : Name) (budget : Nat) (c : Cursor)
      : Selection × Cursor :=
    let (byte, c) := c.next
    let conditions := validTypeConditions parentType
    match budget with
    | 0 =>
        let (slotByte, c) := c.next
        let (directiveByte, c) := c.next
        let rawSlot := slotByte % responseSlots
        let slotIndex :=
          if slotSelectable (slotAt slots rawSlot) parentType 0 then
            rawSlot
          else
            firstSelectableSlot slots parentType 0 rawSlot
        (mkField slots slotIndex (directivesFor (directiveByte % directiveChoices)) [], c)
    | childBudget + 1 =>
        if isUnion parentType then
          -- Only fragments are legal inside a union, and they must narrow to a concrete type.
          let (choiceByte, c) := c.next
          let typeCondition := (conditions[choiceByte % conditions.length]?).getD "Dog"
          let (directiveByte, c) := c.next
          let (countByte, c) := c.next
          let (children, c) :=
            decodeSelections slots typeCondition childBudget (countByte % 3 + 1) c
          (.inlineFragment (some typeCondition)
            (directivesFor (directiveByte % directiveChoices)) children, c)
        else if byte % 4 == 3 then
          let (choiceByte, c) := c.next
          let choice := choiceByte % (conditions.length + 1)
          let typeCondition := if choice == 0 then none else conditions[choice - 1]?
          let (directiveByte, c) := c.next
          let (countByte, c) := c.next
          let (children, c) :=
            decodeSelections slots (typeCondition.getD parentType) childBudget
              (countByte % 3 + 1) c
          (.inlineFragment typeCondition (directivesFor (directiveByte % directiveChoices))
            children, c)
        else
          let (slotByte, c) := c.next
          let (directiveByte, c) := c.next
          let rawSlot := slotByte % responseSlots
          let slotIndex :=
            if slotSelectable (slotAt slots rawSlot) parentType (childBudget + 1) then
              rawSlot
            else
              firstSelectableSlot slots parentType (childBudget + 1) rawSlot
          let directives := directivesFor (directiveByte % directiveChoices)
          match fieldOutputType parentType (slotAt slots slotIndex).field with
          | some childType =>
              let (countByte, c) := c.next
              let (children, c) :=
                decodeSelections slots childType childBudget (countByte % 3 + 1) c
              (mkField slots slotIndex directives children, c)
          | none => (mkField slots slotIndex directives [], c)
  termination_by (budget, 0, 0)
end

-- The operation is always rooted at `animals`, so every generated selection set is analyzed
-- against `Animal`. Only the variables the selections actually use are declared, because GraphQL
-- rejects an operation that declares an unused one and the Rust side renders it that way; the
-- used set is recovered here from the decoded tree rather than tracked during decoding.
-- Variables referenced from argument position. `SelectionConditions.selectionSetBooleanVariables`
-- recovers the `@skip`/`@include` ones; these live in field arguments, so they need their own
-- walk. The grammar only puts a variable at the top level of an argument value, never nested
-- inside a list or input object, so a direct scan of each argument suffices.
mutual
  def selectionArgumentVariables : Selection -> List Name
    | .field _responseName _fieldName arguments _directives selectionSet =>
        (arguments.filterMap
          fun argument =>
            match argument.value with
            | .variable name => some name
            | _ => none)
        ++ selectionSetArgumentVariables selectionSet
    | .inlineFragment _typeCondition _directives selectionSet =>
        selectionSetArgumentVariables selectionSet

  def selectionSetArgumentVariables : List Selection -> List Name
    | [] => []
    | selection :: rest =>
        selectionArgumentVariables selection ++ selectionSetArgumentVariables rest
end

-- The operation is always rooted at `animals`, so every generated selection set is analyzed
-- against `Animal`. Only the variables the selections actually use are declared, because GraphQL
-- rejects an operation that declares an unused one and the Rust side renders it that way; the
-- used sets are recovered here from the decoded tree rather than tracked while decoding.
def decodeOperation (c : Cursor) : Operation × Cursor :=
  let (slots, c) := decodeSlots 0 responseSlots c
  let (packedBooleans, c) := c.next
  let (packedTyped, c) := c.next
  let (countByte, c) := c.next
  let (children, c) := decodeSelections slots "Animal" nestingBudget (countByte % 3 + 1) c
  let selectionSet := [Selection.field "animals" "animals" [] [] children]
  let usedBooleans := (SelectionConditions.selectionSetBooleanVariables selectionSet).eraseDups
  let usedArguments := (selectionSetArgumentVariables selectionSet).eraseDups
  let booleanDefinitions :=
    usedBooleans.filterMap
      fun name =>
        match ["v0", "v1", "v2"].idxOf? name with
        | none => none
        | some index =>
            some (variableDefinition name
              (packedBooleans / variableDeclarations ^ index % variableDeclarations))
  let intDefinitions :=
    usedArguments.filterMap
      fun name =>
        match intVariables.idxOf? name with
        | none => none
        | some index =>
            some (intVariableDefinition name
              (packedTyped / intDeclarations ^ index % intDeclarations))
  let stringDefinitions :=
    usedArguments.filterMap
      fun name =>
        match stringVariables.idxOf? name with
        | none => none
        | some _index =>
            some (stringVariableDefinition name (packedTyped / 9 % stringDeclarations))
  ({
    variableDefinitions := booleanDefinitions ++ intDefinitions ++ stringDefinitions
    selectionSet
  }, c)

def decodeCase (bytes : Array Nat) : Operation × Operation :=
  let c : Cursor := { bytes, pos := 0 }
  let (left, c) := decodeOperation c
  let (right, _c) := decodeOperation c
  (left, right)

-----------------------------------------------------------------------------------------
-- Protocol
-----------------------------------------------------------------------------------------

def hexDigit? (ch : Char) : Option Nat :=
  if ch.isDigit then some (ch.toNat - '0'.toNat)
  else if 'a' ≤ ch && ch ≤ 'f' then some (ch.toNat - 'a'.toNat + 10)
  else if 'A' ≤ ch && ch ≤ 'F' then some (ch.toNat - 'A'.toNat + 10)
  else none

def parseHex (text : String) : Array Nat :=
  let chars := text.toList
  let rec go : List Char -> Array Nat -> Array Nat
    | high :: low :: rest, acc =>
        match hexDigit? high, hexDigit? low with
        | some h, some l => go rest (acc.push (h * 16 + l))
        | _, _ => acc
    | _, acc => acc
  go chars #[]

def flag (value : Bool) : String := if value then "1" else "0"

-- `String.trim` and `String.drop` changed return types across Lean releases; going through the
-- character list keeps this adapter independent of that churn.
def stripTrailing (text : String) : String :=
  String.ofList (text.toList.reverse.dropWhile (fun ch => ch == '\n' || ch == '\r' || ch == ' ')).reverse

def dropChars (text : String) (count : Nat) : String :=
  String.ofList (text.toList.drop count)

-- Splits a request into its command word and the rest. Matching on a `"command "` prefix instead
-- breaks on an argument-less request: trailing whitespace is stripped from the line first, so a
-- request whose argument is empty arrives with no trailing space and matches nothing.
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
    IO.println s!"ok={schemaDigest}{argumentDigest}"
    (<- IO.getStdout).flush
    loop
  else if command == "includes" then
    let (left, right) := decodeCase (parseHex argument)
    let forward := includesBool schema left right
    let forwardReference := includesBoolReference schema left right
    let backward := includesBool schema right left
    let backwardReference := includesBoolReference schema right left
    IO.println
      s!"ok={flag forward},{flag forwardReference},{flag backward},{flag backwardReference}"
    (<- IO.getStdout).flush
    loop
  else if command == "bench" then
    -- `bench <iterations> <hex>`: time `includesBool` inside this process, so the reported cost
    -- is the model's own, with no request round-trip in it. The accepted count is returned and
    -- accumulated so the loop cannot be optimized away.
    let (iterationText, hexText) := splitCommand argument
    let iterations := iterationText.toNat?.getD 1
    let (left, right) := decodeCase (parseHex hexText)
    let _warm := includesBool schema left right
    let started <- IO.monoNanosNow
    let mut accepted := 0
    for _ in [0:iterations] do
      if includesBool schema left right then
        accepted := accepted + 1
    let finished <- IO.monoNanosNow
    IO.println s!"ok={finished - started},{accepted}"
    (<- IO.getStdout).flush
    loop
  else
    IO.println s!"error=unknown request: {command}"
    (<- IO.getStdout).flush
    loop

end QueryInclusionOracle

def main : IO Unit := QueryInclusionOracle.loop
