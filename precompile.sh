#!/bin/sh

for PACKAGE in opentelemetry router-bridge opentelemetry-otlp opentelemetry-jaeger opentelemetry-prometheus tracing tokio moka mockall tower-http tonic apollo-parser hyper-rustls
do
  cargo build -p $PACKAGE
done
