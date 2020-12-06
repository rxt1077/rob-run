#!/bin/bash

# exit on any error
set -e

# show all commands as run
set -x

GIT_URL=$1

git clone ${GIT_URL}
cd "$(basename ${GIT_URL} .git)"
cd example-midterm
docker-compose up -d
echo "Waiting for system to start..."
sleep 30
docker-compose logs
docker-compose down
