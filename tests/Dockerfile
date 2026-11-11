# Linux test image: enough to exercise the nftables backend end to end.
FROM python:3.12-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      nftables iproute2 curl dnsutils procps ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . /src
ENV PYTHONPATH=/src
CMD ["bash"]
