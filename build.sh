#!/bin/sh
set +e
cd src
$(go env GOPATH)/bin/packr2 2> /dev/null
cd ..
go build -o ./build/zippa-db ./src
cd src
$(go env GOPATH)/bin/packr2 clean
echo -e '\e[32mPackage built!\e[0m'
