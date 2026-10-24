#!/bin/sh
set -eu
cd "$(dirname "$0")"
: "${JAVA_HOME:?Path to JDK 17}"
mkdir -p build/tests
"$JAVA_HOME/bin/javac" --release 8 -d build/tests src/eu/tsilva/homewire/tv/HttpRequest.java tests/HttpRequestTest.java
"$JAVA_HOME/bin/java" -cp build/tests eu.tsilva.homewire.tv.HttpRequestTest
