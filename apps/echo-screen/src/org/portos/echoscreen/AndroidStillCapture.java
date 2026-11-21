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
import java.util.function.BiConsumer;
import java.util.function.Consumer;

/** Adapts Camera2 and the process-wide callback looper to a single JPEG capture. */
final class AndroidStillCapture implements StillCapture, StillCapture.Backend {
    private final Context context;
    private final CameraManager cameras;
    private final Handler handler = CallbackThread.HANDLER;
    private final SingleShot capture = new SingleShot(this, new Scheduler() {
        @Override public long now() { return SystemClock.elapsedRealtime(); }
        @Override public void post(Runnable task) { handler.post(task); }
        @Override public void postDelayed(Runnable task, long delay) { handler.postDelayed(task, delay); }
        @Override public void remove(Runnable task) { handler.removeCallbacks(task); }
    });

    AndroidStillCapture(Context context) {
        this.context = context.getApplicationContext();
        cameras = this.context.getSystemService(CameraManager.class);
    }

    @Override public Cancel start(String id, int timeout, Complete complete) {
        return capture.start(id, timeout, complete);
    }

    // An open callback can arrive after Activity destruction. The callback
    // looper stays available so the cancelled operation can close the device.
    private static final class CallbackThread {
        static final Handler HANDLER = start();

        private static Handler start() {
            HandlerThread thread = new HandlerThread("echo-camera-device");
            thread.start();
            return new Handler(thread.getLooper());
        }
    }

    @Override public boolean permitted() {
        return context.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED;
    }

    @Override public Iterable<Mode> modes(String id) throws Fault {
        if (cameras == null) return Collections.emptyList();
        try {
            CameraCharacteristics properties = cameras.getCameraCharacteristics(id);
            StreamConfigurationMap streams = properties.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            Size[] sizes = streams == null ? null : streams.getOutputSizes(ImageFormat.JPEG);
            if (sizes == null) return Collections.emptyList();
            List<Mode> modes = new ArrayList<>();
            for (Size size : sizes) modes.add(new Mode(size.getWidth(), size.getHeight()));
            return modes;
        } catch (CameraAccessException error) {
            throw access(error.getReason());
        }
    }

    @Override public Reader reader(Mode mode, Runnable ready) {
        JpegReader reader = new JpegReader(ImageReader.newInstance(mode.width, mode.height, ImageFormat.JPEG, 1));
        try {
            reader.images.setOnImageAvailableListener(received -> ready.run(), handler);
            return reader;
        } catch (RuntimeException error) {
            reader.close();
            throw error;
        }
    }

    @Override public void open(String id, Consumer<Device> opened, BiConsumer<Device, Failure> failed) throws Fault {
        try {
            cameras.openCamera(id, new CameraDevice.StateCallback() {
                private DeviceHandle device;

                private DeviceHandle handle(CameraDevice received) {
                    if (device == null) device = new DeviceHandle(received);
                    return device;
                }

                @Override public void onOpened(CameraDevice received) {
                    opened.accept(handle(received));
                }

                @Override public void onDisconnected(CameraDevice received) {
                    failed.accept(handle(received), Failure.CAMERA);
                }

                @Override public void onError(CameraDevice received, int error) {
                    failed.accept(handle(received), error == ERROR_CAMERA_IN_USE || error == ERROR_MAX_CAMERAS_IN_USE
                            ? Failure.BUSY : Failure.CAMERA);
                }
            }, handler);
        } catch (CameraAccessException error) {
            throw access(error.getReason());
        }
    }

    private static final class JpegReader implements Reader {
        private final ImageReader images;

        JpegReader(ImageReader images) {
            this.images = images;
        }

        @Override public Result read() {
            try (Image image = images.acquireNextImage()) {
                if (image == null) return null;
                ByteBuffer buffer = image.getPlanes()[0].getBuffer();
                if (buffer.remaining() > CameraWire.MAX_JPEG) return Result.failure(Failure.CAMERA);
                byte[] bytes = new byte[buffer.remaining()];
                buffer.get(bytes);
                return Result.jpeg(bytes, image.getWidth(), image.getHeight(), System.currentTimeMillis());
            }
        }

        @Override public void close() {
            images.close();
        }
    }

    private final class DeviceHandle implements Device {
        private final CameraDevice device;

        DeviceHandle(CameraDevice device) {
            this.device = device;
        }

        @Override public void configure(Reader reader, Consumer<Session> ready, Consumer<Session> failed) throws Fault {
            JpegReader jpeg = (JpegReader) reader;
            try {
                device.createCaptureSession(Collections.singletonList(jpeg.images.getSurface()),
                        new CameraCaptureSession.StateCallback() {
                    private SessionHandle session;

                    private SessionHandle handle(CameraCaptureSession received) {
                        if (session == null) session = new SessionHandle(device, received, jpeg);
                        return session;
                    }

                    @Override public void onConfigured(CameraCaptureSession received) {
                        ready.accept(handle(received));
                    }

                    @Override public void onConfigureFailed(CameraCaptureSession received) {
                        failed.accept(handle(received));
                    }
                }, handler);
            } catch (CameraAccessException error) {
                throw access(error.getReason());
            }
        }

        @Override public void close() {
            device.close();
        }
    }

    private final class SessionHandle implements Session {
        private final CameraDevice device;
        private final CameraCaptureSession session;
        private final JpegReader jpeg;

        SessionHandle(CameraDevice device, CameraCaptureSession session, JpegReader jpeg) {
            this.device = device;
            this.session = session;
            this.jpeg = jpeg;
        }

        @Override public void expose(Runnable failed) throws Fault {
            try {
                CaptureRequest.Builder request = device.createCaptureRequest(CameraDevice.TEMPLATE_STILL_CAPTURE);
                request.addTarget(jpeg.images.getSurface());
                session.capture(request.build(), new CameraCaptureSession.CaptureCallback() {
                    @Override public void onCaptureFailed(CameraCaptureSession owner,
                            CaptureRequest request, CaptureFailure failure) {
                        failed.run();
                    }
                }, handler);
            } catch (CameraAccessException error) {
                throw access(error.getReason());
            }
        }

        @Override public void close() {
            session.close();
        }
    }

    private static Fault access(int reason) {
        return new Fault(reason == CameraAccessException.CAMERA_IN_USE || reason == CameraAccessException.MAX_CAMERAS_IN_USE
                ? Failure.BUSY : Failure.CAMERA);
    }
}
