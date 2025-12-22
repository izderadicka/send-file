#!/bin/bash

cargo build

PROGRAM=./target/debug/send-file

# start sender in the background
RUST_LOG=warn $PROGRAM send ./test-data/audio.webm &
PID=$!

trap "kill -SIGINT $PID" EXIT

sleep 1

echo sender started with pid $PID

# read ticket
TICKET=$(cat ./test-data/file_ticket.txt)
echo Ticket: $TICKET


# start receiver
RUST_LOG=info $PROGRAM receive $TICKET ./test-data/received.webm

echo receiver done

if ! diff ./test-data/audio.webm ./test-data/received.webm; then
    echo "Files are different"
    exit 1
fi

ls -l ./test-data/received.webm
rm ./test-data/received.webm

# kill sender
kill -SIGINT $PID
# pkill send-file