-- Оперативное состояние движка. Живёт локально в SQLite, наружу не выходит.
--
-- Всё, что здесь лежит, обновляется тысячи раз за прогон: статусы, попытки,
-- локи, расход токенов. В Nexorium такой профиль нагрузки невозможен —
-- одиночная правка записи там идёт со скоростью ~100 записей в минуту.

create table if not exists sessions (
    id          text    primary key,
    kind        text    not null,             -- 'houses' | 'staff' | ...
    spec        text    not null,             -- json: исходная постановка задачи
    status      text    not null,             -- pending|planning|running|paused|failed|done
    seed        integer not null,             -- сид сэмплера: даёт воспроизводимость
    budget_usd  real,                         -- null = без потолка
    spent_usd   real    not null default 0,
    created_at  integer not null,             -- unix millis
    updated_at  integer not null
);

create table if not exists jobs (
    id           integer primary key autoincrement,
    session_id   text    not null references sessions(id) on delete cascade,
    kind         text    not null,            -- 'house_text' | 'house_image' | ...
    natural_key  text    not null unique,     -- идемпотентность постановки в очередь
    payload      text    not null,            -- json: готовые параметры от сэмплера
    status       text    not null default 'pending',  -- pending|running|done|failed|dead
    attempts     integer not null default 0,
    max_attempts integer not null default 5,
    locked_by    text,
    locked_at    integer,
    last_error   text,
    result       text,                        -- json: запись, готовая к отправке в Nexorium
    batch_id     text,                        -- в какую пачку отправки вошло задание
    tokens_in    integer not null default 0,
    tokens_out   integer not null default 0,
    cost_usd     real    not null default 0,
    created_at   integer not null,
    updated_at   integer not null
);

-- Основной индекс под захват: (сессия, статус, порядок выдачи).
create index if not exists jobs_claim_idx on jobs (session_id, status, id);
-- Под reaper: поиск протухших локов.
create index if not exists jobs_reap_idx on jobs (status, locked_at);
-- Под сборку пачек: что готово к отправке и ещё не отправлено.
create index if not exists jobs_batch_idx on jobs (session_id, status, batch_id);

-- Пачки отправки в Nexorium.
--
-- Существуют только ради грабли №5: обрыв соединения на POST не означает, что
-- сервер запрос не выполнил. Статус 'unknown' разрешается сверкой по batch_id.
create table if not exists batches (
    id          text    primary key,          -- uuid, уходит в поле batch_id записей
    session_id  text    not null references sessions(id) on delete cascade,
    collection  text    not null,
    expected    integer not null,             -- сколько записей должно лечь
    status      text    not null,             -- pending|inflight|unknown|committed|failed
    attempts    integer not null default 0,
    last_error  text,
    created_at  integer not null,
    updated_at  integer not null
);

create index if not exists batches_status_idx on batches (status, session_id);
