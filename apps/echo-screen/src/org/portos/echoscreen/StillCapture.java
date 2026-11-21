package org.portos.echoscreen;

import java.util.ArrayDeque;
import java.util.Deque;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.BiConsumer;
import java.util.function.Consumer;

interface StillCapture {
    interface Cancel {
        void close();
    }

    interface Complete {
        void result(Result result);
    }

    Cancel start(String id, int timeout, Complete complete);

    interface Scheduler {
        long now();
        void post(Runnable task);
        void postDelayed(Runnable task, long delay);
        void remove(Runnable task);
    }

    interface Backend {
        boolean permitted();
        Iterable<Mode> modes(String id) throws Fault;
        Reader reader(Mode mode, Runnable ready);
        void open(String id, Consumer<Device> opened, BiConsumer<Device, Failure> failed) throws Fault;
    }

    interface Resource extends AutoCloseable {
        @Override void close();
    }

    interface Reader extends Resource {
        Result read();
    }

    interface Device extends Resource {
        void configure(Reader reader, Consumer<Session> ready, Consumer<Session> failed) throws Fault;
    }

    interface Session extends Resource {
        void expose(Runnable failed) throws Fault;
    }

    final class Fault extends Exception {
        private static final long serialVersionUID = 1L;
        final Failure failure;

        Fault(Failure failure) {
            this.failure = failure;
        }
    }

    /** Runs the single capture and owns its resources on the callback thread. */
    final class SingleShot implements StillCapture {
        private final Backend backend;
        private final Scheduler scheduler;

        SingleShot(Backend backend, Scheduler scheduler) {
            this.backend = backend;
            this.scheduler = scheduler;
        }

        @Override public Cancel start(String id, int timeout, Complete complete) {
            Operation operation = new Operation(scheduler.now() + timeout, complete);
            scheduler.post(() -> operation.open(id));
            return operation::cancel;
        }

        private final class Operation {
            private final long expires;
            private final Complete complete;
            private final Lifetime lifetime = new Lifetime();
            private final AtomicBoolean cancelled = new AtomicBoolean();
            private final Runnable deadline = () -> fail(Failure.TIMEOUT);
            private Reader reader;
            private Device device;
            private Session session;

            Operation(long expires, Complete complete) {
                this.expires = expires;
                this.complete = complete;
            }

            private boolean live() {
                if (!lifetime.closed() && !cancelled.get() && scheduler.now() < expires) return true;
                fail(Failure.TIMEOUT);
                return false;
            }

            void open(String id) {
                step(() -> {
                    if (!backend.permitted()) {
                        fail(Failure.PERMISSION);
                        return;
                    }
                    Mode mode = Mode.largest(backend.modes(id));
                    if (mode == null) {
                        fail(Failure.CAMERA);
                        return;
                    }
                    if (!live()) return;
                    reader = backend.reader(mode, this::image);
                    lifetime.keep(reader);
                    if (!live()) return;
                    scheduler.postDelayed(deadline, Math.max(0, expires - scheduler.now()));
                    backend.open(id, this::opened, this::deviceFailed);
                });
            }

            private void opened(Device acquired) {
                if (device == acquired) return;
                if (!live() || device != null) {
                    Lifetime.release(acquired);
                    return;
                }
                device = acquired;
                lifetime.keep(acquired);
                step(() -> acquired.configure(reader, this::configured, this::sessionFailed));
            }

            private void configured(Session acquired) {
                if (session == acquired) return;
                if (!live() || session != null) {
                    Lifetime.release(acquired);
                    return;
                }
                session = acquired;
                lifetime.keep(acquired);
                step(() -> acquired.expose(() -> fail(Failure.CAMERA)));
            }

            private void deviceFailed(Device failed, Failure failure) {
                if (device != failed) Lifetime.release(failed);
                fail(failure);
            }

            private void sessionFailed(Session failed) {
                if (session != failed) Lifetime.release(failed);
                fail(Failure.CAMERA);
            }

            private void image() {
                step(() -> {
                    if (session == null) return;
                    Result result = reader.read();
                    if (result != null) finish(result);
                });
            }

            private void step(Action action) {
                if (!live()) return;
                try {
                    action.run();
                } catch (Fault fault) {
                    fail(fault.failure);
                } catch (SecurityException denied) {
                    fail(Failure.PERMISSION);
                } catch (RuntimeException error) {
                    fail(Failure.CAMERA);
                }
            }

            private void fail(Failure failure) {
                finish(Result.failure(failure));
            }

            void cancel() {
                cancelled.set(true);
                scheduler.post(() -> finish(null));
            }

            private void finish(Result result) {
                if (!lifetime.close()) return;
                scheduler.remove(deadline);
                if (!cancelled.get() && result != null) {
                    complete.result(scheduler.now() < expires ? result : Result.failure(Failure.TIMEOUT));
                }
            }
        }

        private interface Action {
            void run() throws Fault;
        }
    }

    enum Failure {
        BUSY("busy"), PERMISSION("permission_denied"), CAMERA("camera_error"), TIMEOUT("timeout");
        final String wire;

        Failure(String wire) {
            this.wire = wire;
        }
    }

    /** Owned by the camera callback thread. Late acquisitions are closed on arrival. */
    final class Lifetime {
        private final Deque<AutoCloseable> resources = new ArrayDeque<>();
        private boolean closed;

        void keep(Resource resource) {
            resources.push(resource);
        }

        boolean closed() {
            return closed;
        }

        boolean close() {
            if (closed) return false;
            closed = true;
            while (!resources.isEmpty()) release(resources.pop());
            return true;
        }

        static void release(AutoCloseable resource) {
            try {
                resource.close();
            } catch (Exception ignored) {
                // Every other resource must still be released.
            }
        }
    }

    final class Result {
        final byte[] jpeg;
        final int width;
        final int height;
        final long sampledAt;
        final Failure failure;

        private Result(byte[] jpeg, int width, int height, long sampledAt, Failure failure) {
            this.jpeg = jpeg;
            this.width = width;
            this.height = height;
            this.sampledAt = sampledAt;
            this.failure = failure;
        }

        static Result jpeg(byte[] data, int width, int height, long sampledAt) {
            if (width < 1 || width > 1920 || height < 1 || height > 1080
                    || data.length < 1 || data.length > CameraWire.MAX_JPEG || sampledAt < 0) {
                return failure(Failure.CAMERA);
            }
            return new Result(data, width, height, sampledAt, null);
        }

        static Result failure(Failure failure) {
            return new Result(null, 0, 0, 0, failure);
        }
    }

    final class Mode {
        final int width;
        final int height;

        Mode(int width, int height) {
            this.width = width;
            this.height = height;
        }

        static Mode largest(Iterable<Mode> modes) {
            Mode best = null;
            for (Mode mode : modes) {
                if (mode.width < 1 || mode.width > 1920 || mode.height < 1 || mode.height > 1080) continue;
                int area = mode.width * mode.height;
                if (best == null || area > best.width * best.height
                        || (area == best.width * best.height && mode.width > best.width)) best = mode;
            }
            return best;
        }
    }
}
