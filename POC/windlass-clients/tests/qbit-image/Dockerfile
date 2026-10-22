FROM lscr.io/linuxserver/qbittorrent:5.2.0

RUN mkdir -p /config/qBittorrent \
    && chown -R abc:abc /config

COPY --chown=abc:abc qBittorrent.conf /config/qBittorrent/qBittorrent.conf
