# synthforge: образ для сервера.
#
# Сборка в два шага: компилятор и зависимости остаются в первом слое, в
# рабочий образ попадают только бинарник и данные, которые он читает при
# работе (словари, справочники, каталог моделей).

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --bin forge

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=build /src/target/release/forge /usr/local/bin/forge
COPY dictionaries ./dictionaries
COPY reference ./reference
COPY config ./config

# Очередь сессий и локальная копия результатов живут на томе: без него
# перезапуск контейнера терял бы возобновляемость прогонов.
ENV SYNTHFORGE_DB=/data/synthforge.db \
    SYNTHFORGE_OUT=/data/out \
    PANEL_BIND=0.0.0.0
VOLUME ["/data"]

EXPOSE 8787
ENTRYPOINT ["forge"]
CMD ["panel", "8787"]
