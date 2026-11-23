package org.portos.echoscreen;

import android.app.Activity;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.SharedPreferences;
import android.graphics.Color;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.UserManager;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.webkit.RenderProcessGoneDetail;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;
import android.widget.TextView;

/** Thin Android adapters. No URL, intent, error description or token is logged. */
public final class MainActivity extends Activity implements ScreenLoad.Port {
    private final Handler handler = new Handler(Looper.getMainLooper());
    private ScreenLoad load;
    private FrameLayout frame;
    private TextView message;
    private WebView web;
    private CameraPairing pairing;
    private AndroidStillCapture camera;
    private NativeCameraClient nativeCamera;
    private boolean started;
    private boolean listeningForUnlock;
    private final BroadcastReceiver unlockReceiver = new BroadcastReceiver() {
        @Override public void onReceive(Context context, Intent intent) {
            if (Intent.ACTION_USER_UNLOCKED.equals(intent.getAction())) checkUnlock();
        }
    };

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        frame = new FrameLayout(this);
        frame.setBackgroundColor(Color.BLACK);
        // Raw physical pixels: hassmic's grip and light band overlay the app.
        frame.setPadding(0, ScreenConfig.TOP, 0, ScreenConfig.BOTTOM);
        message = new TextView(this);
        message.setTextColor(Color.WHITE);
        message.setBackgroundColor(Color.BLACK);
        message.setGravity(Gravity.CENTER);
        message.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_POLITE);
        frame.addView(message, fill());
        setContentView(frame);
        load = new ScreenLoad(this);
        startLoad(getIntent());
        if (!userUnlocked()) {
            registerReceiver(unlockReceiver, new IntentFilter(Intent.ACTION_USER_UNLOCKED));
            listeningForUnlock = true;
        }
        checkUnlock();
        immersive();
    }

    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        startLoad(intent);
    }

    private void startLoad(Intent intent) {
        load.start(() -> configure(intent));
    }

    @Override public boolean userUnlocked() {
        UserManager user = getSystemService(UserManager.class);
        return user != null && user.isUserUnlocked();
    }

    private void checkUnlock() {
        if (!userUnlocked()) return;
        stopListeningForUnlock();
        load.onUserUnlocked();
    }

    private void stopListeningForUnlock() {
        if (listeningForUnlock) {
            unregisterReceiver(unlockReceiver);
            listeningForUnlock = false;
        }
    }

    private ScreenConfig configure(Intent intent) {
        stopCamera();
        SharedPreferences preferences = getSharedPreferences("echo-screen", MODE_PRIVATE);
        ScreenConfig.Store store = new ScreenConfig.Store() {
            public Secret read() {
                String value = preferences.getString("url", null);
                return value == null ? null : new Secret(value);
            }
            public void save(Secret url) {
                preferences.edit().putString("url", url.expose())
                        .putBoolean("camera_trusted", CameraPairing.parse(url) != null).apply();
            }
        };
        String extra = intent.getStringExtra("url");
        boolean supplied = intent.hasExtra("url");
        ScreenConfig config = ScreenConfig.select(supplied, extra == null ? null : new Secret(extra), store);
        if (supplied && config.url == null) preferences.edit().putBoolean("camera_trusted", false).apply();
        pairing = preferences.getBoolean("camera_trusted", false) ? CameraPairing.parse(config.url) : null;
        startCamera();
        return config;
    }

    @Override protected void onStart() {
        super.onStart();
        started = true;
        startCamera();
    }

    @Override protected void onStop() {
        started = false;
        stopCamera();
        super.onStop();
    }

    private void startCamera() {
        if (started && pairing != null && nativeCamera == null) {
            if (camera == null) camera = new AndroidStillCapture(this);
            nativeCamera = new NativeCameraClient(pairing, camera);
            nativeCamera.start();
        }
    }

    private void stopCamera() {
        if (nativeCamera != null) {
            nativeCamera.close();
            nativeCamera = null;
        }
    }

    private FrameLayout.LayoutParams fill() {
        return new FrameLayout.LayoutParams(-1, -1);
    }

    private void immersive() {
        getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY | View.SYSTEM_UI_FLAG_FULLSCREEN
                | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION | View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
    }

    @Override public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (focused) immersive();
    }

    @Override public void cancel() {
        clearSchedule();
        WebView view = web;
        if (view != null) {
            view.stopLoading();
            discard(view);
        }
    }

    private void discard(WebView view) {
        web = null;
        frame.removeView(view);
        view.destroy();
    }

    @Override public void waiting(String text) {
        if (!text.contentEquals(message.getText())) message.setText(text);
        message.setVisibility(View.VISIBLE);
        if (web != null) web.setVisibility(View.INVISIBLE);
        message.bringToFront();
    }

    @Override public void load(Secret url, long attempt) {
        // A fresh view per attempt isolates late Android callbacks from older loads.
        web = new WebView(this);
        web.setVisibility(View.INVISIBLE);
        web.getSettings().setJavaScriptEnabled(true);
        web.getSettings().setDomStorageEnabled(true);
        web.setWebViewClient(new WebViewClient() {
            @Override public void onReceivedError(WebView view, WebResourceRequest request,
                    WebResourceError error) {
                load.error(attempt, request.isForMainFrame());
            }
            @Override public void onReceivedHttpError(WebView view, WebResourceRequest request,
                    WebResourceResponse response) {
                load.error(attempt, request.isForMainFrame());
            }
            @Override public void onPageFinished(WebView view, String ignored) {
                load.finished(attempt);
            }
            @Override public boolean onRenderProcessGone(WebView view, RenderProcessGoneDetail detail) {
                if (view == web) {
                    discard(view);
                    load.rendererGone(attempt);
                }
                return true;
            }
        });
        frame.addView(web, fill());
        message.bringToFront();
        web.loadUrl(url.expose());
    }

    @Override public void schedule(long delay, Runnable retry) {
        handler.postDelayed(retry, delay);
    }

    @Override public void clearSchedule() {
        handler.removeCallbacksAndMessages(null);
    }

    @Override public void showPage() {
        message.setVisibility(View.GONE);
        if (web != null) web.setVisibility(View.VISIBLE);
    }

    @Override protected void onDestroy() {
        stopListeningForUnlock();
        stopCamera();
        if (load != null) load.destroy();
        super.onDestroy();
    }
}
