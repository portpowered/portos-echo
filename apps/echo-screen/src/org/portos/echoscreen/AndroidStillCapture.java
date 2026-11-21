package org.portos.echoscreen;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureFailure;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.util.Size;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;

/** Camera2 resources and callbacks remain on a single owning thread. */
final class AndroidStillCapture implements StillCapture {
    private final Context context;
    private final CameraManager cameras;
    private final Handler handler = CallbackThread.HANDLER;

    AndroidStillCapture(Context context) {
        this.context = context.getApplicationContext();
        cameras = this.context.getSystemService(CameraManager.class);
    }

    @Override public Cancel start(String id, int timeout, Complete complete) {
        Operation capture = new Operation(id, SystemClock.elapsedRealtime() + timeout, complete);
        handler.post(capture::open);
        return capture::cancel;
    }

    // Android can deliver an open callback after Activity destruction.
    // Keep one process-wide callback looper so the cancelled operation closes it.
    private static final class CallbackThread {
        static final Handler HANDLER = start();

        private static Handler start() {
            HandlerThread thread = new HandlerThread("echo-camera-device");
            thread.start();
            return new Handler(thread.getLooper());
        }
    }

    private final class Operation {
        final String id;
        final long expires;
        final Complete complete;
        final AtomicBoolean cancelled = new AtomicBoolean();
        final Lifetime lifetime = new Lifetime();
        final Runnable deadline = () -> fail(Failure.TIMEOUT);
        CameraDevice device;
        CameraCaptureSession session;
        ImageReader reader;

        Operation(String id, long expires, Complete complete) {
            this.id = id;
            this.expires = expires;
            this.complete = complete;
        }

        boolean live() {
            return !lifetime.closed() && !cancelled.get() && SystemClock.elapsedRealtime() < expires;
        }

        void open() {
            if (!live()) {
                fail(Failure.TIMEOUT);
                return;
            }
            if (context.checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
                fail(Failure.PERMISSION);
                return;
            }
            try {
                if (cameras == null) {
                    fail(Failure.CAMERA);
                    return;
                }
                Mode mode = mode(cameras.getCameraCharacteristics(id));
                if (mode == null) {
                    fail(Failure.CAMERA);
                    return;
                }
                if (!live()) {
                    fail(Failure.TIMEOUT);
                    return;
                }
                reader = ImageReader.newInstance(mode.width, mode.height, ImageFormat.JPEG, 1);
                lifetime.keep(reader);
                reader.setOnImageAvailableListener(this::image, handler);
                handler.postDelayed(deadline, Math.max(0, expires - SystemClock.elapsedRealtime()));
                cameras.openCamera(id, deviceCallbacks(), handler);
            } catch (SecurityException denied) {
                fail(Failure.PERMISSION);
            } catch (CameraAccessException error) {
                fail(access(error.getReason()));
            } catch (RuntimeException error) {
                fail(Failure.CAMERA);
            }
        }

        CameraDevice.StateCallback deviceCallbacks() {
            return new CameraDevice.StateCallback() {
                @Override public void onOpened(CameraDevice opened) {
                    if (!live()) {
                        Lifetime.release(opened);
                        fail(Failure.TIMEOUT);
                        return;
                    }
                    device = opened;
                    lifetime.keep(opened);
                    configure();
                }

                @Override public void onDisconnected(CameraDevice disconnected) {
                    if (device != disconnected) Lifetime.release(disconnected);
                    fail(Failure.CAMERA);
                }

                @Override public void onError(CameraDevice failed, int error) {
                    if (device != failed) Lifetime.release(failed);
                    fail(error == ERROR_CAMERA_IN_USE || error == ERROR_MAX_CAMERAS_IN_USE
                            ? Failure.BUSY : Failure.CAMERA);
                }
            };
        }

        void configure() {
            try {
                device.createCaptureSession(Collections.singletonList(reader.getSurface()),
                        new CameraCaptureSession.StateCallback() {
                    @Override public void onConfigured(CameraCaptureSession configured) {
                        if (!live()) {
                            Lifetime.release(configured);
                            fail(Failure.TIMEOUT);
                            return;
                        }
                        session = configured;
                        lifetime.keep(configured);
                        exposeOnce();
                    }

                    @Override public void onConfigureFailed(CameraCaptureSession failed) {
                        Lifetime.release(failed);
                        fail(Failure.CAMERA);
                    }
                }, handler);
            } catch (SecurityException denied) {
                fail(Failure.PERMISSION);
            } catch (CameraAccessException error) {
                fail(access(error.getReason()));
            } catch (RuntimeException error) {
                fail(Failure.CAMERA);
            }
        }

        void exposeOnce() {
            if (!lifetime.beginExposure()) return;
            try {
                CaptureRequest.Builder request = device.createCaptureRequest(CameraDevice.TEMPLATE_STILL_CAPTURE);
                request.addTarget(reader.getSurface());
                session.capture(request.build(), new CameraCaptureSession.CaptureCallback() {
                    @Override public void onCaptureFailed(CameraCaptureSession owner,
                            CaptureRequest request, CaptureFailure failure) {
                        fail(Failure.CAMERA);
                    }
                }, handler);
            } catch (SecurityException denied) {
                fail(Failure.PERMISSION);
            } catch (CameraAccessException error) {
                fail(access(error.getReason()));
            } catch (RuntimeException error) {
                fail(Failure.CAMERA);
            }
        }

        void image(ImageReader received) {
            if (!live()) {
                fail(Failure.TIMEOUT);
                return;
            }
            Result result;
            try (Image image = received.acquireNextImage()) {
                if (image == null) return;
                ByteBuffer buffer = image.getPlanes()[0].getBuffer();
                if (buffer.remaining() > CameraWire.MAX_JPEG) {
                    result = Result.failure(Failure.CAMERA);
                } else {
                    byte[] bytes = new byte[buffer.remaining()];
                    buffer.get(bytes);
                    result = Result.jpeg(bytes, image.getWidth(), image.getHeight(), System.currentTimeMillis());
                }
            } catch (RuntimeException error) {
                result = Result.failure(Failure.CAMERA);
            }
            finish(result);
        }

        void fail(Failure failure) {
            finish(Result.failure(failure));
        }

        void cancel() {
            cancelled.set(true);
            handler.post(() -> finish(null));
        }

        void finish(Result result) {
            if (!lifetime.close()) return;
            handler.removeCallbacks(deadline);
            if (!cancelled.get() && result != null) {
                complete.result(SystemClock.elapsedRealtime() < expires ? result : Result.failure(Failure.TIMEOUT));
            }
        }
    }

    private static Mode mode(CameraCharacteristics properties) {
        StreamConfigurationMap streams = properties.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
        Size[] sizes = streams == null ? null : streams.getOutputSizes(ImageFormat.JPEG);
        if (sizes == null) return null;
        List<Mode> modes = new ArrayList<>();
        for (Size size : sizes) modes.add(new Mode(size.getWidth(), size.getHeight()));
        return Mode.largest(modes);
    }

    private static Failure access(int reason) {
        return reason == CameraAccessException.CAMERA_IN_USE || reason == CameraAccessException.MAX_CAMERAS_IN_USE
                ? Failure.BUSY : Failure.CAMERA;
    }
}
