# Postgres with the ja_JP.UTF-8 locale generated, for the yen-locale database
# the money live test needs (`live_postgres_money_scales_by_the_servers_locale_not_always_by_100`).
# A plain `postgres:16` image does not carry it (see that test's doc comment).
FROM postgres:16

RUN localedef -i ja_JP -c -f UTF-8 -A /usr/share/locale/locale.alias ja_JP.UTF-8

COPY initdb/ /docker-entrypoint-initdb.d/
