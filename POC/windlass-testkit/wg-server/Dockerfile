FROM alpine:3.21
RUN apk add --no-cache wireguard-tools iproute2 python3
COPY entrypoint.sh /opt/entrypoint.sh
COPY services.py /opt/services.py
RUN chmod +x /opt/entrypoint.sh
ENTRYPOINT ["/opt/entrypoint.sh"]
