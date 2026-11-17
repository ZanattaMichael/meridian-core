# The Meridian container image: the meridian command and every plugin.
#
# The release workflow builds it from the two static Linux archives it has
# just made, so the image holds the same signed binaries as the downloads.
# The build context is a directory with one unpacked archive per platform:
#
#   amd64/   the contents of meridian-<version>-x86_64-unknown-linux-musl
#   arm64/   the contents of meridian-<version>-aarch64-unknown-linux-musl
#
# The image compiles and validates documents as it stands. Planning and
# applying run the target's own tool, which the image does not carry: build an
# image of your own FROM this one and add the tools you use, for example
#
#   FROM ghcr.io/zanattamichael/meridian:0.1.0
#   USER root
#   RUN apk add --no-cache opentofu
#   USER meridian
FROM alpine:3.22

ARG TARGETARCH
ARG VERSION=0.0.0

LABEL org.opencontainers.image.title="Meridian" \
      org.opencontainers.image.description="One document, any tool: compile Meridian documents and run the tools they target" \
      org.opencontainers.image.source="https://github.com/ZanattaMichael/meridian-core" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}"

# The command and its plugins stay in one directory, where meridian looks.
COPY ${TARGETARCH}/ /opt/meridian/
RUN ln -s /opt/meridian/meridian /usr/local/bin/meridian \
    && adduser -D -H -u 10001 meridian \
    && mkdir /work && chown meridian /work

USER meridian
WORKDIR /work
ENTRYPOINT ["meridian"]
CMD ["--help"]
