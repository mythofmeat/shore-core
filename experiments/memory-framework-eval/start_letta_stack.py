#!/usr/bin/env python3
"""Boot an embedded Postgres (pgserver) + Letta REST server pointed at it.

Letta 0.16.7 requires Postgres for the server runtime (sqlite is only wired
in ORM column dispatch). To keep this contained in the experiments/ dir and
avoid any system-level changes, we use pgserver, which vendors a postgres
binary and runs it out of a local pgdata/ directory.

Run from experiments/memory-framework-eval/. Blocks; SIGINT to stop.
"""
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load_dotenv():
    for p in [Path.home()/"Documents/qifei/config/.env", Path.home()/".config/shore/.env"]:
        if not p.exists():
            continue
        for line in p.read_text().splitlines():
            s = line.strip()
            if not s or s.startswith("#") or "=" not in s:
                continue
            k, v = s.split("=", 1)
            k = k.strip(); v = v.strip()
            if v.startswith(("'", '"')):
                q = v[0]; end = v.find(q, 1)
                if end != -1: v = v[1:end]
            else:
                for i, ch in enumerate(v):
                    if ch == "#" and i > 0 and v[i-1] in " \t":
                        v = v[:i]; break
                v = v.strip()
            if k and k not in os.environ:
                os.environ[k] = v
        return


_load_dotenv()

key = os.environ.get("OPENROUTER_SHORE_PRIMARY") or os.environ.get("OPENROUTER_API_KEY")
if not key:
    print("ERROR: no OpenRouter key in env", file=sys.stderr)
    sys.exit(1)

os.environ["OPENROUTER_API_KEY"] = key
os.environ.setdefault("OPENAI_API_KEY", key)
# Embeddings — OpenRouter proxies OpenAI embedding models, so point the
# OpenAI-compat path at OpenRouter.
os.environ.setdefault("OPENAI_API_BASE", "https://openrouter.ai/api/v1")

# asyncpg falls back to the OS user when a URI netloc omits a hostname
# (e.g. "postgresql://postgres:@/letta?host=/sock"). Force the user via env.
os.environ["PGUSER"] = "postgres"
os.environ["PGPASSWORD"] = ""

letta_dir = HERE / ".letta"
letta_dir.mkdir(exist_ok=True)
os.environ["LETTA_DIR"] = str(letta_dir)

# Boot embedded Postgres. pgserver defaults to Unix-socket only; we force TCP
# so pg8000 (used for the one-shot schema bootstrap) and asyncpg (used by the
# Letta server) can both reach it via host=127.0.0.1.
import pgserver
pgdata = HERE / ".letta-pgdata"
print(f"[stack] starting embedded postgres at {pgdata}")
server = pgserver.get_server(str(pgdata), cleanup_mode="stop")
socket_dir = str(pgdata)  # pgserver listens on its own unix socket here

# URI for Letta. pgserver binds only on a Unix socket (its default config
# hardcodes `-h ""` on boot). asyncpg reaches it via ?host=<socket_dir>;
# psycopg2 does the same with host=<socket_dir>; SQLAlchemy's pg8000 dialect
# does NOT handle that, so we use psycopg2 for the sync bootstrap path.
letta_pg_uri_async = f"postgresql+asyncpg://postgres:@/letta?host={socket_dir}"
letta_pg_uri_sync = f"postgresql+psycopg2://postgres@/letta?host={socket_dir}"
# Letta's db.py will see our LETTA_PG_URI and run its own driver rewrite.
# Passing an asyncpg-flavoured URI lets convert_to_async_uri leave it alone.
os.environ["LETTA_PG_URI"] = letta_pg_uri_async
print(f"[stack] LETTA_PG_URI (async)={letta_pg_uri_async}")
print(f"[stack] bootstrap URI (sync)={letta_pg_uri_sync}")

# Create the letta database + pgvector extension. Use psycopg2 directly so we
# don't need to spawn psql (which would demand TCP, which pgserver disables).
import psycopg2  # type: ignore
from psycopg2 import sql as _sql, errors as _pgerr  # type: ignore

_admin = psycopg2.connect(
    dbname="postgres", user="postgres", host=socket_dir,
)
_admin.autocommit = True
with _admin.cursor() as cur:
    cur.execute("SELECT 1 FROM pg_database WHERE datname = %s;", ("letta",))
    if cur.fetchone():
        print("[stack] database 'letta' already exists")
    else:
        cur.execute("CREATE DATABASE letta;")
        print("[stack] created database 'letta'")
_admin.close()

_letta = psycopg2.connect(
    dbname="letta", user="postgres", host=socket_dir,
)
_letta.autocommit = True
with _letta.cursor() as cur:
    cur.execute("CREATE EXTENSION IF NOT EXISTS vector;")
_letta.close()
print("[stack] pgvector extension ready in 'letta'")

# Letta doesn't ship alembic migrations in the pip wheel (deployment-time
# concern, expected to be handled by their Docker image). For our sidecar
# we bootstrap the schema by importing every ORM module so Base.metadata
# sees all tables, then calling create_all via a sync engine. Safe: first
# run creates, subsequent runs are no-ops.
def _bootstrap_schema():
    from sqlalchemy import create_engine
    from letta.orm.base import Base
    import letta.orm  # noqa: F401 — triggers registration of every Table

    eng = create_engine(letta_pg_uri_sync)
    Base.metadata.create_all(eng)
    eng.dispose()


_bootstrap_schema()
print("[stack] schema bootstrapped via Base.metadata.create_all")


# Letta's alembic migrations (not shipped in the wheel) add DB-level sequences
# for monotonic columns like messages.sequence_id. Without them,
# FetchedValue() has nothing to fetch and inserts fail with NOT NULL. Patch
# the missing sequences by hand — idempotent.
def _install_sequences():
    _c = psycopg2.connect(
        dbname="letta", user="postgres", host=socket_dir,
    )
    _c.autocommit = True
    with _c.cursor() as cur:
        # messages.sequence_id
        cur.execute("""
        CREATE SEQUENCE IF NOT EXISTS messages_sequence_id_seq OWNED BY messages.sequence_id;
        ALTER TABLE messages
            ALTER COLUMN sequence_id SET DEFAULT nextval('messages_sequence_id_seq');
        """)
    _c.close()


_install_sequences()
print("[stack] installed missing sequences for FetchedValue columns")

# Launch Letta server as a subprocess so pgserver's refcount stays on this
# parent process (it cleans up when this script exits).
letta_bin = HERE / ".venv-letta" / "bin" / "letta"
cmd = [str(letta_bin), "server", "--host", "127.0.0.1", "--port", "8283"]
print(f"[stack] launching: {' '.join(cmd)}")
proc = subprocess.Popen(cmd, env=os.environ.copy())


def _shutdown(signum, frame):
    print(f"\n[stack] received signal {signum}, shutting down")
    try:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
    finally:
        pass
    sys.exit(0)


signal.signal(signal.SIGINT, _shutdown)
signal.signal(signal.SIGTERM, _shutdown)

print(f"[stack] letta pid={proc.pid}")
while True:
    rc = proc.poll()
    if rc is not None:
        print(f"[stack] letta server exited with {rc}")
        sys.exit(rc)
    time.sleep(1)
