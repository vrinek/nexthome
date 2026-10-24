-- The ten tables from PLAN.md "Data model", in dependency order.
-- SQLite has no datetime or json types: ISO-8601 timestamps and JSON both
-- live in TEXT columns.

CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    created_at TEXT
);

CREATE TABLE members (
    id INTEGER PRIMARY KEY,
    account_id INTEGER,
    email TEXT,
    api_key_hash TEXT,
    created_at TEXT,
    UNIQUE (email),
    FOREIGN KEY (account_id) REFERENCES accounts (id)
);
CREATE INDEX idx_members_account_id ON members (account_id);

CREATE TABLE relocations (
    id INTEGER PRIMARY KEY,
    account_id INTEGER,
    city_name TEXT,
    lat REAL,
    lon REAL,
    zoom REAL,
    paid_at TEXT,
    expires_at TEXT,
    archived_at TEXT,
    stripe_checkout_session_id TEXT,
    FOREIGN KEY (account_id) REFERENCES accounts (id)
);
CREATE INDEX idx_relocations_account_id ON relocations (account_id);

CREATE TABLE place_types (
    id INTEGER PRIMARY KEY,
    relocation_id INTEGER,
    name TEXT,
    icon TEXT,
    color TEXT,
    search_type TEXT,
    search_keyword TEXT,
    UNIQUE (relocation_id, name),
    UNIQUE (relocation_id, icon),
    UNIQUE (relocation_id, color),
    FOREIGN KEY (relocation_id) REFERENCES relocations (id)
);
CREATE INDEX idx_place_types_relocation_id ON place_types (relocation_id);

CREATE TABLE locations (
    id INTEGER PRIMARY KEY,
    relocation_id INTEGER,
    place_type_id INTEGER,
    member_id INTEGER,
    name TEXT,
    lat REAL,
    lon REAL,
    state TEXT CHECK (state IN ('inbox', 'rejected', 'accepted', 'contacted', 'waitlisted', 'booked')),
    note TEXT,
    source_url TEXT,
    external_identifier TEXT,
    google_maps_payload TEXT,
    created_at TEXT,
    updated_at TEXT,
    UNIQUE (relocation_id, source_url),
    UNIQUE (relocation_id, external_identifier),
    FOREIGN KEY (relocation_id) REFERENCES relocations (id),
    FOREIGN KEY (place_type_id) REFERENCES place_types (id),
    FOREIGN KEY (member_id) REFERENCES members (id)
);
CREATE INDEX idx_locations_relocation_id ON locations (relocation_id);
CREATE INDEX idx_locations_place_type_id ON locations (place_type_id);
CREATE INDEX idx_locations_member_id ON locations (member_id);

CREATE TABLE isochrones (
    id INTEGER PRIMARY KEY,
    location_id INTEGER,
    minutes INTEGER,
    geojson TEXT,
    UNIQUE (location_id, minutes),
    FOREIGN KEY (location_id) REFERENCES locations (id)
);
CREATE INDEX idx_isochrones_location_id ON isochrones (location_id);

CREATE TABLE evaluations (
    id INTEGER PRIMARY KEY,
    location_id INTEGER,
    computed_at TEXT,
    rows TEXT,
    FOREIGN KEY (location_id) REFERENCES locations (id)
);
CREATE INDEX idx_evaluations_location_id ON evaluations (location_id);

CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    kind TEXT,
    payload TEXT,
    run_after TEXT,
    attempts INTEGER,
    last_error TEXT,
    done_at TEXT
);

CREATE TABLE api_usage (
    id INTEGER PRIMARY KEY,
    relocation_id INTEGER,
    provider TEXT CHECK (provider IN ('mapbox', 'google')),
    month INTEGER,
    calls INTEGER,
    UNIQUE (relocation_id, provider, month),
    FOREIGN KEY (relocation_id) REFERENCES relocations (id)
);
CREATE INDEX idx_api_usage_relocation_id ON api_usage (relocation_id);

CREATE TABLE issues (
    id INTEGER PRIMARY KEY,
    member_id INTEGER,
    body TEXT,
    created_at TEXT,
    FOREIGN KEY (member_id) REFERENCES members (id)
);
CREATE INDEX idx_issues_member_id ON issues (member_id);
