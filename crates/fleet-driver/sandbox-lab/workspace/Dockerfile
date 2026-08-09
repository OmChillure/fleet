# Project / dev environment container (NOT the harness).
# Installs language toolchains and runs clone + project_setup on a shared volume.
# Harness images stay stable; this image may grow per language matrix.
FROM python:3.13-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
      git \
      openssh-client \
      curl \
      ca-certificates \
      build-essential \
      libffi-dev \
      libjpeg-dev \
      zlib1g-dev \
      libfreetype6-dev \
      libpng-dev \
      # WeasyPrint / cairo stack used by some Django apps (e.g. 123-invoices)
      libcairo2 \
      libpango-1.0-0 \
      libpangocairo-1.0-0 \
      libgdk-pixbuf-2.0-0 \
      shared-mime-info \
      fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 1000 -m -s /bin/bash fleet \
    && mkdir -p /workspace /workspace/.fleet \
    && chown -R fleet:fleet /workspace

COPY workspace_agent.py /usr/local/bin/workspace_agent.py
RUN chmod 755 /usr/local/bin/workspace_agent.py

# Node optional for Playwright evidence later — keep image smaller for MVP
# RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && apt-get install -y nodejs

WORKDIR /workspace
ENV FLEET_DIR=/workspace/.fleet \
    PYTHONUNBUFFERED=1

# Run as root for package installs, then chown tree to 1000 for harness user
USER root
ENTRYPOINT ["python3", "/usr/local/bin/workspace_agent.py"]
