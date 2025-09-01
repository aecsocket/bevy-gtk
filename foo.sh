#!/usr/bin/env bash
# usage: ./fourcc_decode.sh 942948929

num="$1"

# convert decimal -> hex, pad to 8 digits
hex=$(printf "%08x\n" "$num")

# split into bytes and convert each to ASCII char (little-endian)
fourcc=$(echo "$hex" | sed 's/../& /g' | awk '{for(i=4;i>=1;i--) printf "%c", strtonum("0x"$i)}')

echo "$fourcc"

