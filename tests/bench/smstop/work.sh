# SPDX-License-Identifier: Apache-2.0
# Runs in the guest: syscall-heavy work and one compute-bound control, best
# of five each, one `name seconds` line per item.
set -e
best() {
    local name=$1 b=0 s e
    shift
    for _ in 1 2 3 4 5; do
        s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N)
        (( b == 0 || e - s < b )) && b=$(( e - s ))
    done
    printf '%s %d.%06d\n' "$name" $(( b / 1000000000 )) $(( b % 1000000000 / 1000 ))
}
find /usr >/dev/null 2>&1
head -c 67108864 /dev/urandom > /tmp/smstop-rand
best find-usr find /usr
best dd-4k dd if=/dev/zero of=/dev/null bs=4k count=200000
best tar-usr-lib tar cf /dev/null /usr/lib
best exec-1000 sh -c 'for i in $(seq 1000); do /bin/true; done'
best python-50 sh -c 'for i in $(seq 50); do python3 -c pass; done'
best gzip-64m gzip -1 -c /tmp/smstop-rand
rm -f /tmp/smstop-rand
