#!/bin/sh

for PACKAGE in router-bridge opentelemetry opentelemetry-otlp opentelemetry-jaeger opentelemetry-prometheus tracing tokio moka mockall tower-http tonic hyper-rustls
do
  cargo build -p $PACKAGE
done
