# MyTerm sync server: standard-library Python, no third-party packages.
# ssh-keygen (openssh-client) signs SSH certificates when they're turned on.
FROM python:3.13-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-client \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin myterm \
 && mkdir -p /data && chown myterm /data

WORKDIR /app
COPY myterm_sync ./myterm_sync

USER myterm
ENV PYTHONUNBUFFERED=1 \
    MYTERM_SYNC_DB=/data/sync.db \
    MYTERM_SYNC_PORT=7341
VOLUME /data
EXPOSE 7341

HEALTHCHECK --interval=60s --timeout=5s --retries=3 \
  CMD python -c "import urllib.request,os,ssl; s='https' if os.environ.get('MYTERM_SYNC_TLS_CERT') else 'http'; urllib.request.urlopen(f'{s}://127.0.0.1:7341/v1/health', context=ssl._create_unverified_context(), timeout=4)"

ENTRYPOINT ["python", "-m", "myterm_sync"]
