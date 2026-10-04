# A jump host for the SSH tunnel live tests
# (`live_postgres_connects_through_an_ssh_tunnel` and friends): OpenSSH with
# password sign-in and TCP forwarding on, so a tunnel through it reaches the
# compose network's `postgres` and `mysql` services by name.
FROM alpine:3.20

# Alpine's own sshd_config turns forwarding off, and sshd takes the first
# value it reads for a keyword, so the shipped lines are removed rather than
# overridden by an appended one.
RUN apk add --no-cache openssh \
    && ssh-keygen -A \
    && adduser -D -s /bin/sh tunnel \
    && echo 'tunnel:secret' | chpasswd \
    && sed -i -E '/^[[:space:]]*#?[[:space:]]*(AllowTcpForwarding|PasswordAuthentication|PermitRootLogin)[[:space:]]/d' \
        /etc/ssh/sshd_config \
    && printf 'PasswordAuthentication yes\nAllowTcpForwarding yes\nPermitRootLogin no\n' \
        >> /etc/ssh/sshd_config \
    && /usr/sbin/sshd -t

EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
