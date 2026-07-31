package com.apollographql.router.examples;

import org.teavm.jso.JSExport;

public final class JavaHeader {
    private JavaHeader() {}

    @JSExport
    public static String headerValue() {
        return "active";
    }
}
