package org.portos.echoscreen;

import java.util.ArrayDeque;
import java.util.Deque;

interface StillCapture {
    interface Cancel {
        void close();
    }

    interface Complete {
        void result(Result result);
    }

    Cancel start(String id, int timeout, Complete complete);

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
        private boolean exposing;
        private boolean closed;

        boolean keep(AutoCloseable resource) {
            if (closed) {
                release(resource);
                return false;
            }
            resources.push(resource);
            return true;
        }

        boolean beginExposure() {
            if (closed || exposing || resources.size() != 3) return false;
            exposing = true;
            return true;
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
