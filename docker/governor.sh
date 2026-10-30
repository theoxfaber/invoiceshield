#!/bin/sh
# Container entrypoint for the governor server. Durable state defaults to the /data volume.
# (Set here, not as an image ENV, so other commands in the image, such as the offline demo whose
# eval-mode governors must use an in-memory DB, do not inherit it.)
: "${GOVERNOR_DB:=/data/governor.db}"
export GOVERNOR_DB
# Phase 2: the governor needs the extraction service's PUBLIC key. Give it directly in
# GOVERNOR_EXTRACTOR_PUBKEY, or point GOVERNOR_EXTRACTOR_PUBKEY_FILE at a file holding the hex key
# (docker-compose's extractor-key service writes one). The private key never enters this container.
if [ -z "${GOVERNOR_EXTRACTOR_PUBKEY:-}" ] && [ -n "${GOVERNOR_EXTRACTOR_PUBKEY_FILE:-}" ]; then
  if [ -r "$GOVERNOR_EXTRACTOR_PUBKEY_FILE" ]; then
    GOVERNOR_EXTRACTOR_PUBKEY=$(tr -d ' \n\r\t' < "$GOVERNOR_EXTRACTOR_PUBKEY_FILE")
    export GOVERNOR_EXTRACTOR_PUBKEY
  else
    echo "GOVERNOR_EXTRACTOR_PUBKEY_FILE=$GOVERNOR_EXTRACTOR_PUBKEY_FILE is not readable" >&2
    exit 2
  fi
fi
exec /app/governor/target/release/governor-server "$@"
