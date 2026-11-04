CREATE TABLE bookings (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL,
    flight_id uuid NOT NULL,
    passenger_name text NOT NULL CHECK (length(trim(passenger_name)) BETWEEN 1 AND 200),
    passenger_email text NOT NULL CHECK (length(passenger_email) BETWEEN 3 AND 254),
    seat_count integer NOT NULL CHECK (seat_count > 0),
    total_price bigint NOT NULL CHECK (total_price > 0),
    currency text NOT NULL CHECK (currency = 'RUB'),
    status text NOT NULL CHECK (status IN ('CONFIRMED','CANCELLED')),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX bookings_user ON bookings(user_id, created_at);
-- Internal durable saga journal, not a partially created Booking.
CREATE TABLE booking_operations (
    booking_id uuid PRIMARY KEY,
    request jsonb NOT NULL,
    state text NOT NULL CHECK (state IN ('PENDING','COMPENSATE','DONE','ABORTED')),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- A cancellation is replayed after a crash between remote release and local commit.
CREATE TABLE cancellation_operations (
    booking_id uuid PRIMARY KEY REFERENCES bookings(id),
    created_at timestamptz NOT NULL DEFAULT now()
);
