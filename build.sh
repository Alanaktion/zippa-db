#!/bin/sh
set -e
go build -o ./build/zippa-db ./src
echo -e '\e[32mPackage built!\e[0m'
