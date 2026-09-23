-- Обратная связь по результатам генерации.
--
-- Генерация на старте — не производство, а эксперимент. Нельзя запустить
-- тысячу штук и посмотреть результат в конце: надо смотреть по ходу, ставить
-- на паузу и помечать конкретные результаты — «фон плохой», «все угрюмые»,
-- «вылизанный».
--
-- Пометки живут в базе, а не в переписке, потому что они вход для доработки
-- словаря. Замечание «все фотки угрюмые», повторённое двадцать раз, — это
-- измеримый сигнал добавить параметр жизнерадостности с диапазоном. Устное
-- впечатление таким сигналом не становится и теряется.

create table if not exists annotations (
    id          integer primary key autoincrement,
    session_id  text    not null references sessions(id) on delete cascade,
    -- К какому заданию относится. null — замечание ко всему прогону.
    job_id      integer references jobs(id) on delete cascade,
    -- Что именно помечено: 'text' | 'image' | 'entity'
    target      text    not null,
    -- Путь к файлу или иной адрес помеченного результата.
    asset       text,
    -- 'good' | 'bad'
    verdict     text    not null,
    -- json-массив коротких меток: ["фон","угрюмый"]. По ним считается сводка.
    tags        text    not null default '[]',
    comment     text,
    -- Кто пометил. Пригодится, когда приёмку делают несколько человек.
    author      text,
    created_at  integer not null
);

create index if not exists annotations_session_idx on annotations (session_id, target);
create index if not exists annotations_job_idx on annotations (job_id);
create index if not exists annotations_verdict_idx on annotations (session_id, verdict);

-- Файлы, порождённые заданиями: портреты, интерьеры, территория.
--
-- Отдельной таблицей, потому что у одной сущности их несколько, а помечать
-- надо каждый по отдельности.
create table if not exists assets (
    id          integer primary key autoincrement,
    session_id  text    not null references sessions(id) on delete cascade,
    -- Ключ сущности, а не задания.
    --
    -- Текст и картинки одной сущности делаются разными заданиями и в разное
    -- время: у них разная стоимость, разные лимиты и разный профиль отказов.
    -- Свести их при сборке записи можно только по общему ключу сущности.
    entity_key  text    not null,
    job_id      integer references jobs(id) on delete set null,
    -- Назначение: 'portrait' | 'room' | 'territory' | ...
    role        text    not null,
    path        text    not null,
    bytes       integer not null default 0,
    cost_usd    real    not null default 0,
    created_at  integer not null
);

create index if not exists assets_session_idx on assets (session_id, role);
create unique index if not exists assets_entity_role_idx on assets (entity_key, role);
