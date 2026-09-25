"""Extract the example / counter-example blocks of each validation rule in
Section 4 of the GraphQL Federation spec into a JSON fixture."""
import json, re, subprocess, sys

spec_dir = sys.argv[1]
out = sys.argv[2]
commit = subprocess.check_output(["git", "-C", spec_dir, "rev-parse", "--short", "HEAD"]).decode().strip()
lines = open(f"{spec_dir}/spec/Section 4 -- Composition.md").read().split("\n")

def split_schemas(body_lines):
    schemas, current, result_section = [], None, False
    for line in body_lines:
        header = re.match(r"^#\s*(Source Schema|Schema)\s+([A-Z])\b", line, re.I)
        if header:
            current = [header.group(2), []]
            schemas.append(current)
            result_section = False
            continue
        if re.match(r"^#\s*(Composed Result|Composite Schema|Schema Composition|Merged)", line):
            result_section = True
            continue
        if result_section:
            continue
        if current is None:
            current = ["A", []]
            schemas.append(current)
        current[1].append(line)
    # Some examples show two schemas without labelling them: split where a type is defined a
    # second time.
    split = []
    for name, body in schemas:
        current, defined = [], set()
        for line in body:
            definition = re.match(r"^(?:extend\s+)?(?:type|interface|union|enum|input|scalar)\s+([A-Za-z_][A-Za-z0-9_]*)", line)
            if definition and not line.startswith("extend") and definition.group(1) in defined:
                split.append([current])
                current, defined = [], set()
            if definition and not line.startswith("extend"):
                defined.add(definition.group(1))
            current.append(line)
        split.append([current])
    letters = "ABCDEFGH"
    named = []
    for index, (body,) in enumerate(split):
        text = "\n".join(body).strip()
        if text:
            named.append((f"Schema{letters[len(named)]}", text))
    return named

cases = []
rule = code = None
i = 0
while i < len(lines):
    line = lines[i]
    heading = re.match(r"^(#{2,5}) (.+)$", line)
    if heading:
        if len(heading.group(1)) == 4:
            rule, code = heading.group(2).strip(), None
        elif len(heading.group(1)) < 4:
            rule, code = None, None
        i += 1
        continue
    code_match = re.match(r"^`([A-Z_]+)`$", line)
    if code_match and rule is not None and code is None:
        code = code_match.group(1)
        i += 1
        continue
    fence = re.match(r"^```graphql (example|counter-example)\s*$", line)
    if fence:
        kind = fence.group(1)
        body = []
        i += 1
        while i < len(lines) and not lines[i].startswith("```"):
            body.append(lines[i])
            i += 1
        if rule is not None and code is not None:
            index = sum(1 for c in cases if c["rule"] == rule and c["kind"] == kind)
            cases.append({"rule": rule, "code": code, "kind": kind, "index": index,
                          "schemas": split_schemas(body)})
    i += 1

json.dump({
    "source": f"graphql/graphql-federation-spec@{commit}, spec/Section 4 -- Composition.md (MIT License, Copyright (c) GraphQL Contributors)",
    "cases": cases,
}, open(out, "w"), indent=1)
print(len(cases), "cases,", len({c['rule'] for c in cases}), "rules")
