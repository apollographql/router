// String-heavy Rhai script for benchmarking string interning performance.
//
// This script is intentionally designed to exercise the Rhai string interner
// frequently: each iteration of the loop performs string concatenation and map
// key lookups, both of which acquire the RwLock on the StringsInterner when
// the `sync` feature is enabled.
//
// The goal is to produce a measurable difference between default interning
// (256 strings, RwLock acquired per op) and disabled interning (0, no lock).

fn supergraph_service(service) {
    let request_callback = Fn("process_request");
    service.map_request(request_callback);
}

fn process_request(request) {
    let prefix = "x-rhai-bench-";
    let i = 0;
    while i < 20 {
        let key = prefix + to_string(i);
        let val = "bench-value-" + to_string(i) + "-done";
        request.headers[key] = val;
        i += 1;
    }
    request.context["rhai_bench_complete"] = "true";
}
