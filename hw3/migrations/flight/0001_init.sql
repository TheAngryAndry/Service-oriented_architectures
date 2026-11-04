CREATE TABLE airlines (
    code text PRIMARY KEY CHECK (code ~ '^[A-Z0-9]{2,3}$'),
    name text NOT NULL CHECK (length(trim(name)) > 0)
);
CREATE TABLE airports (
    code text PRIMARY KEY CHECK (code ~ '^[A-Z]{3}$'),
    name text NOT NULL
);
CREATE TABLE flights (
    id uuid PRIMARY KEY,
    flight_number text NOT NULL,
    airline_code text NOT NULL REFERENCES airlines(code),
    origin text NOT NULL REFERENCES airports(code),
    destination text NOT NULL REFERENCES airports(code),
    departure_time timestamptz NOT NULL,
    arrival_time timestamptz NOT NULL,
    total_seats integer NOT NULL CHECK (total_seats > 0),
    available_seats integer NOT NULL CHECK (available_seats >= 0 AND available_seats <= total_seats),
    price_minor bigint NOT NULL CHECK (price_minor > 0),
    currency text NOT NULL DEFAULT 'RUB' CHECK (currency = 'RUB'),
    status text NOT NULL CHECK (status IN ('SCHEDULED','DEPARTED','CANCELLED','COMPLETED')),
    CHECK (origin <> destination),
    CHECK (arrival_time > departure_time)
);
CREATE UNIQUE INDEX flight_number_day ON flights(flight_number, ((departure_time AT TIME ZONE 'UTC')::date));
CREATE INDEX flight_search ON flights(origin, destination, departure_time) WHERE status = 'SCHEDULED';
CREATE TABLE seat_reservations (
    id uuid PRIMARY KEY,
    flight_id uuid NOT NULL REFERENCES flights(id),
    booking_id uuid NOT NULL UNIQUE,
    seat_count integer NOT NULL CHECK (seat_count > 0),
    status text NOT NULL CHECK (status IN ('ACTIVE','RELEASED','EXPIRED')),
    created_at timestamptz NOT NULL DEFAULT now()
);
-- A fence prevents a delayed ReserveSeats from running after compensation.
CREATE TABLE released_booking_ids (booking_id uuid PRIMARY KEY);
-- Committed in the same transaction as every flight mutation.
CREATE TABLE cache_revision (singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton), revision bigint NOT NULL);
INSERT INTO cache_revision VALUES (true, 0);
INSERT INTO airlines VALUES ('SU', 'Aeroflot'), ('S7', 'S7 Airlines');
INSERT INTO airports VALUES ('SVO','Sheremetyevo'),('LED','Pulkovo'),('VKO','Vnukovo'),('DME','Domodedovo');
INSERT INTO flights VALUES
('11111111-1111-4111-8111-111111111111','SU1000','SU','SVO','LED','2027-04-01T09:00:00Z','2027-04-01T10:30:00Z',100,100,550000,'RUB','SCHEDULED'),
('22222222-2222-4222-8222-222222222222','SU1002','SU','SVO','LED','2027-04-01T15:00:00Z','2027-04-01T16:30:00Z',1,1,700000,'RUB','SCHEDULED'),
('33333333-3333-4333-8333-333333333333','S71001','S7','DME','LED','2027-04-02T09:00:00Z','2027-04-02T10:30:00Z',50,50,600000,'RUB','SCHEDULED'),
('44444444-4444-4444-8444-444444444444','SU1004','SU','SVO','LED','2027-04-01T18:00:00Z','2027-04-01T19:30:00Z',50,50,650000,'RUB','CANCELLED');
