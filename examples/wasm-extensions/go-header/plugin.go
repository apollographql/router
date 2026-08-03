package export_apollo_router_plugin_hooks

import (
	witTypes "go.bytecodealliance.org/pkg/wit/types"
	. "wit_component/apollo_router_plugin_hooks"
)

func Handle(Event) witTypes.Result[Outcome, string] {
	mutation := Mutation{
		Headers: []HeaderOperation{
			MakeHeaderOperationSet(Header{
				Name:   "x-wasm-go",
				Values: []string{"active"},
			}),
		},
		Context: []ContextOperation{
			MakeContextOperationSet(ContextEntry{
				Name:  "wasm.go",
				Value: `{"language":"go"}`,
			}),
		},
		Method: witTypes.None[string](),
		Uri:    witTypes.None[string](),
		Body:   witTypes.None[string](),
	}
	return witTypes.Ok[Outcome, string](MakeOutcomeProceed(mutation))
}
