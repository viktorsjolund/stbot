FROM --platform=linux/arm64 rust:1.83.0-bookworm

ARG TWITCH_OAUTH_TOKEN
ARG TWITCH_BOT_NICK
ARG SPOTIFY_CLIENT_ID
ARG SPOTIFY_CLIENT_SECRET
ARG WEB_URI
ARG AMQP_ADDR

WORKDIR /usr/src/myapp
COPY . .

ENV TWITCH_OAUTH_TOKEN=$TWITCH_OAUTH_TOKEN
ENV TWITCH_BOT_NICK=$TWITCH_BOT_NICK
ENV SPOTIFY_CLIENT_ID=$SPOTIFY_CLIENT_ID
ENV SPOTIFY_CLIENT_SECRET=$SPOTIFY_CLIENT_SECRET
ENV WEB_URI=$WEB_URI
ENV AMQP_ADDR=$AMQP_ADDR

RUN cargo install --path . --target=aarch64-unknown-linux-gnu 

CMD ["rust-ws"]
