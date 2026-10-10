# A git daemon serving the e2e app repository at git://git-server.argocd.svc/app.git
FROM alpine:3.20
RUN apk add --no-cache git git-daemon
COPY repo /srv/src
RUN git config --global user.email e2e@example.com && git config --global user.name e2e \
 && cd /srv/src && git init -q -b main && git add -A && git commit -qm "e2e app" \
 && mkdir -p /srv/git && git clone -q --bare /srv/src /srv/git/app.git
EXPOSE 9418
CMD ["git", "daemon", "--export-all", "--base-path=/srv/git", "--reuseaddr", "--informative-errors", "/srv/git"]
