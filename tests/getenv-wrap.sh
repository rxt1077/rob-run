#!/bin/bash

# exit on any error
set -e

# show all commands as run
set -x

GIT_URL=$1

git clone ${GIT_URL}
cd "$(basename ${GIT_URL} .git)"
./build.sh
./test
