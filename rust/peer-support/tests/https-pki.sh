#!/bin/sh
# Generate throwaway credentials for the loopback rustls and reverse-proxy tests.
set -eu
umask 077
cd "$1"
for ca in client backend foreign; do
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
        -keyout "$ca.key" -out "$ca.pem" -days 2 -subj "/CN=$ca CA" \
        -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign
done
issue() {
    name=$1 ca=$2 purpose=$3 days=$4
    openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
        -keyout "$name.key" -out leaf.csr -subj "/CN=$name"
    dns=localhost
    if [ "$name" = other-server ]; then dns=other.example.invalid; fi
    printf '%s\n' basicConstraints=critical,CA:FALSE keyUsage=critical,digitalSignature \
        "extendedKeyUsage=$purpose" "subjectAltName=DNS:$dns" > leaf.ext
    case "$days" in
        expired) set -- -not_before 20200101000000Z -not_after 20210101000000Z ;;
        short)
            expiry=$(date -u -d '+4 seconds' +%s)
            printf '%s\n' "$expiry" > short.expiry
            set -- -not_after "$(date -u -d "@$expiry" +%Y%m%d%H%M%SZ)"
            ;;
        *) set -- -days "$days" ;;
    esac
    openssl x509 -req -in leaf.csr -CA "$ca.pem" -CAkey "$ca.key" \
        -CAcreateserial -out "$name.pem" "$@" -extfile leaf.ext
}
issue server backend serverAuth 2
issue other-server backend serverAuth 2
for client in cli nginx apache unlisted; do issue "$client" client clientAuth 1; done
issue expired client clientAuth expired
issue wrong client serverAuth 1
issue stranger foreign clientAuth 1
issue short client clientAuth short
