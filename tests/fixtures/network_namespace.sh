#!/bin/sh
set -eu
# This script runs exclusively in the test's fresh outer user/network namespace.
ip link set lo up
ip addr add 192.168.65.1/32 dev lo
ip -6 addr add fdc5:1234::1/128 dev lo nodad
exec setpriv --inh-caps=-all --ambient-caps=-all -- /usr/bin/python3 -B "$@"
