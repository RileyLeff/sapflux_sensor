#!/bin/sh
# /usr/local/bin/sapflux: run the admin CLI as the service user so the
# database and window files keep the right owner.
if [ "$(id -un)" = sapflux ]; then
    exec /usr/local/lib/sapflux/sapflux "$@"
fi
exec sudo -u sapflux /usr/local/lib/sapflux/sapflux "$@"
