FROM node:22-bookworm-slim
ARG CODEX_VERSION=0.154.0
ARG CLAUDE_VERSION=2.1.273
RUN apt-get update && apt-get install -y --no-install-recommends python3 git ripgrep ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @openai/codex@${CODEX_VERSION} @anthropic-ai/claude-code@${CLAUDE_VERSION} \
    && npm cache clean --force \
    && groupadd -g 10001 agent && useradd -u 10001 -g 10001 -m agent
COPY entrypoint.py /opt/nectra/entrypoint.py
USER 10001:10001
ENV HOME=/home/agent
WORKDIR /home/agent
ENTRYPOINT ["python3", "/opt/nectra/entrypoint.py"]
