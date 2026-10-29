package eu.tsilva.homewire.betty;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.app.KeyguardManager;
import android.app.role.RoleManager;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.provider.Settings;
import android.speech.RecognitionListener;
import android.speech.RecognizerIntent;
import android.speech.SpeechRecognizer;
import android.text.InputFilter;
import android.text.InputType;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.Gravity;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowManager;
import android.view.inputmethod.InputMethodManager;
import android.widget.*;
import org.drinkless.tdlib.TdApi;
import java.util.ArrayList;
import java.util.Locale;

public final class MainActivity extends Activity implements Telegram.Listener {
    private static final int BG=0xff101b19, PANEL=0xff182623, TEXT=0xffedf4f2;
    private static final int MUTED=0xff8eaaa3, MINT=0xffa7e2c9;
    private final Handler ui=new Handler(Looper.getMainLooper());
    private final Draft draft=new Draft();
    private final Handoff handoff=new Handoff();
    private Telegram telegram;
    private LinearLayout root, composer;
    private TextView timer, hint, status, partial;
    private EditText textInput;
    private SpeechRecognizer recognizer;
    private int speechGeneration, duration;
    private long started;
    private boolean foreground, openRequest=true, permissionAsked, preview, captureVisible;
    private boolean pendingLast, unknownLast, handoffBusy, messageSent;
    private String screen="", typed="", speechIssue="";
    private Draft.State rendered;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        preview=BuildFlags.PREVIEW;
        telegram=((BettyApplication)getApplication()).telegram;
        getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        if(preview) { buildCapture();draft.start();duration=8;renderComposer(); }
        else if(telegram==null) fatal(((BettyApplication)getApplication()).failure);
        else { telegram.listener=this;changed(); }
    }
    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);setIntent(intent);openRequest=true;
        if(draft.state==Draft.State.REVIEW||draft.state==Draft.State.TEXT) toast("Your unsent message is still here.");
        maybeRecord();
    }
    @Override protected void onResume() {
        super.onResume();foreground=true;
        if(telegram!=null) { telegram.listener=this;telegram.setActive(true);changed(); }
        maybeRecord();
    }
    @Override public void onWindowFocusChanged(boolean focus) { super.onWindowFocusChanged(focus);if(focus)maybeRecord(); }
    @Override protected void onPause() {
        foreground=false;
        if(!preview&&(draft.state==Draft.State.RECORDING||recognizer!=null)) {
            if(draft.state==Draft.State.RECORDING){duration=elapsed();draft.stop();}
            releaseSpeech();renderComposer();
        }
        ui.removeCallbacks(tick);
        if(telegram!=null)telegram.setActive(false);
        super.onPause();
    }
    @Override protected void onDestroy() {
        ui.removeCallbacksAndMessages(null);releaseSpeech();
        if(telegram!=null&&telegram.listener==this)telegram.listener=null;
        super.onDestroy();
    }
    @Override public void onBackPressed() {
        if(draft.state==Draft.State.RECORDING&&!preview) { stopRecording();return; }
        if(draft.state==Draft.State.REVIEW||draft.state==Draft.State.TEXT) {
            new AlertDialog.Builder(this).setTitle("Discard this message?")
                .setNegativeButton("Keep",null).setPositiveButton("Discard",(d,w)->{discard();finish();}).show();return;
        }
        super.onBackPressed();
    }
    @Override public void delivered() {
        if(handoff.confirmed()) { messageSent=true;openRequest=false; }
        changed();
    }
    @Override public void changed() {
        if(isFinishing()||telegram==null)return;
        if(!telegram.ready) { getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);renderLogin();return; }
        getWindow().clearFlags(WindowManager.LayoutParams.FLAG_SECURE);
        if(!captureVisible)buildCapture();
        status.setText(telegram.status);
        boolean notice=!telegram.status.isEmpty()&&!telegram.sendBusy&&!telegram.status.equals("Sent to Telegram");
        status.setVisibility(notice?View.VISIBLE:View.GONE);
        if(telegram.sendUnknown) {
            if(draft.state==Draft.State.RECORDING)stopRecording();
            draft.state=Draft.State.UNCERTAIN;
        } else if(telegram.sendBusy) {
            draft.state=Draft.State.SENDING;
        } else if((pendingLast||unknownLast)&&!telegram.sendBusy) {
            draft.state=Draft.State.IDLE;typed="";
            if(textInput!=null)textInput.setText("");
            // Acknowledging an uncertain result is not a delivery receipt.
            if(!handoff.ready())handoff.abandon();
        }
        pendingLast=telegram.sendBusy;unknownLast=telegram.sendUnknown;
        if(rendered!=draft.state)renderComposer();
        if(handoff.take(foreground)) { openRequest=false;openTelegram(true); }
        else maybeRecord();
    }
    private void base() {
        root=column();root.setBackgroundColor(BG);
        root.setOnApplyWindowInsetsListener((view,insets)->{
            android.graphics.Insets bars=insets.getInsets(WindowInsets.Type.systemBars()|WindowInsets.Type.displayCutout());
            android.graphics.Insets ime=insets.getInsets(WindowInsets.Type.ime());
            view.setPadding(bars.left,bars.top,bars.right,Math.max(bars.bottom,ime.bottom));return insets;
        });
        setContentView(root);root.requestApplyInsets();
    }
    private void buildCapture() {
        captureVisible=true;screen="capture";base();
        LinearLayout header=row();header.setPadding(dp(24),dp(8),dp(12),0);
        header.addView(text("Betty",20,TEXT,true),new LinearLayout.LayoutParams(0,-2,1));
        header.addView(icon(R.drawable.ic_more,"Settings",()->showMenu()),box(48,48));root.addView(header);
        status=text("",13,MINT,false);status.setPadding(dp(24),dp(12),dp(24),0);status.setVisibility(View.GONE);root.addView(status);
        composer=column();composer.setPadding(dp(24),dp(24),dp(24),dp(20));
        root.addView(composer,new LinearLayout.LayoutParams(-1,0,1));
        root.setFocusableInTouchMode(true);root.requestFocus();renderComposer();
    }
    private void renderComposer() {
        if(composer==null)return;
        if(textInput!=null)typed=textInput.getText().toString();
        textInput=null;partial=null;rendered=draft.state;composer.removeAllViews();
        boolean recording=draft.state==Draft.State.RECORDING;
        boolean editing=draft.state==Draft.State.REVIEW||draft.state==Draft.State.TEXT;
        LinearLayout.LayoutParams body=new LinearLayout.LayoutParams(-1,0,1);
        if(editing) {
            textInput=input(recognizer==null?"Type a message…":"Transcribing…",false);
            textInput.setSingleLine(false);textInput.setGravity(Gravity.TOP);
            textInput.setTextSize(24);textInput.setLineSpacing(dp(5),1);
            textInput.setPadding(0,dp(12),0,dp(12));textInput.setBackground(null);
            textInput.setFilters(new InputFilter[]{new InputFilter.LengthFilter(4096)});
            textInput.setText(typed);textInput.setEnabled(recognizer==null);
            composer.addView(textInput,body);
        } else {
            ScrollView transcript=new ScrollView(this);transcript.setFillViewport(true);
            String copy=recording?typed:draft.state==Draft.State.SENDING?typed:
                draft.state==Draft.State.UNCERTAIN?"Check the last message in Telegram.":messageSent?"Sent.":"";
            partial=text(copy,24,TEXT,false);partial.setLineSpacing(dp(5),1);
            partial.setPadding(0,dp(12),0,dp(12));partial.setHintTextColor(MUTED);
            if(recording)partial.setHint("Speak a message…");
            transcript.addView(partial);composer.addView(transcript,body);
        }
        hint=text(recording?"Listening · "+clock(duration):recognizer!=null?"Transcribing…":
            draft.state==Draft.State.SENDING?"Sending…":speechIssue,13,MUTED,false);
        hint.setGravity(Gravity.CENTER);hint.setPadding(0,dp(12),0,dp(16));
        hint.setVisibility(hint.getText().length()==0?View.GONE:View.VISIBLE);composer.addView(hint);timer=hint;
        LinearLayout controls=row();controls.setGravity(Gravity.CENTER);controls.setPadding(0,dp(8),0,0);
        if(recording) {
            controls.addView(icon(R.drawable.ic_close,"Cancel",()->discard()),box(48,48));
            controls.addView(primary("Stop",()->{
                if(preview){typed="Could you explain the last point?";draft.stop();renderComposer();}
                else stopRecording();
            }),weightedButton());
            controls.addView(icon(R.drawable.ic_keyboard,"Use keyboard",()->keyboard()),box(48,48));
        } else if(editing) {
            controls.addView(icon(R.drawable.ic_mic,"Record again",()->recordAgain()),box(48,48));
            Button send=primary("Send",()->sendText());controls.addView(send,weightedButton());
            EditText editor=textInput;
            Runnable enable=()->{
                boolean allowed=recognizer==null&&!editor.getText().toString().trim().isEmpty();
                send.setEnabled(allowed);send.setAlpha(allowed?1f:.35f);
            };
            editor.addTextChangedListener(new TextWatcher(){
                public void beforeTextChanged(CharSequence value,int start,int count,int after){}
                public void onTextChanged(CharSequence value,int start,int before,int count){}
                public void afterTextChanged(Editable value){
                    if(textInput!=editor)return;
                    typed=value.toString();enable.run();
                    if(!typed.trim().isEmpty()&&recognizer==null){speechIssue="";hint.setVisibility(View.GONE);}
                }
            });
            enable.run();
        } else if(draft.state==Draft.State.UNCERTAIN) {
            controls.addView(primary("Open Telegram",()->openTelegram(false)),weightedButton());
        } else if(draft.state!=Draft.State.SENDING) {
            controls.addView(primary(messageSent?"Open Telegram":"Record",()->{
                if(messageSent)openTelegram(true);else startRecording();
            }),weightedButton());
            controls.addView(icon(R.drawable.ic_keyboard,"Use keyboard",()->keyboard()),box(48,48));
        }
        composer.addView(controls,new LinearLayout.LayoutParams(-1,-2));
        if(draft.state==Draft.State.UNCERTAIN)composer.addView(secondary("I've checked",()->{
            if(preview){draft.state=Draft.State.IDLE;renderComposer();return;}
            new AlertDialog.Builder(this).setTitle("Checked the chat?")
                .setMessage("This clears the warning without sending again.")
                .setNegativeButton("Back",null).setPositiveButton("Continue",(d,w)->telegram.acknowledgeUncertain()).show();
        }));
    }
    private void recordAgain() {
        Runnable restart=()->{discard();startRecording();};
        String current=textInput==null?typed:textInput.getText().toString();
        if(!current.trim().isEmpty())new AlertDialog.Builder(this).setTitle("Replace this message?")
            .setNegativeButton("Keep",null).setPositiveButton("Record again",(d,w)->restart.run()).show();
        else restart.run();
    }
    private void maybeRecord() {
        if(preview||!foreground||!hasWindowFocus()||!openRequest||handoffBusy||handoff.ready()||telegram==null||!telegram.ready
            ||telegram.sendBusy||telegram.sendUnknown||draft.state!=Draft.State.IDLE)return;
        if(getSystemService(KeyguardManager.class).isDeviceLocked())return;
        openRequest=false;ui.postDelayed(()->{if(foreground&&draft.state==Draft.State.IDLE&&!handoffBusy&&!handoff.ready())startRecording();},200);
    }
    private void startRecording() {
        openRequest=false;
        if(draft.state!=Draft.State.IDLE)return;
        if(preview){typed="";speechIssue="";messageSent=false;draft.start();duration=0;renderComposer();return;}
        if(!foreground||telegram==null||!telegram.ready||telegram.sendBusy||telegram.sendUnknown)return;
        if(getSystemService(KeyguardManager.class).isDeviceLocked())return;
        if(checkSelfPermission(Manifest.permission.RECORD_AUDIO)!=PackageManager.PERMISSION_GRANTED) {
            if(!permissionAsked){permissionAsked=true;requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO},4);}
            else toast("Allow Microphone in Android Settings → Apps → Betty → Permissions.");return;
        }
        if(!SpeechRecognizer.isOnDeviceRecognitionAvailable(this)){speechIssue="On-device dictation unavailable.";renderComposer();return;}
        if(!draft.start())return;
        typed="";speechIssue="";messageSent=false;started=SystemClock.elapsedRealtime();duration=0;
        try {
            recognizer=SpeechRecognizer.createOnDeviceSpeechRecognizer(this);final int generation=++speechGeneration;
            recognizer.setRecognitionListener(new RecognitionListener(){
                public void onReadyForSpeech(Bundle p){}
                public void onBeginningOfSpeech(){}
                public void onRmsChanged(float rms){}
                public void onBufferReceived(byte[] audio){}
                public void onEndOfSpeech(){if(generation!=speechGeneration)return;duration=elapsed();if(draft.state==Draft.State.RECORDING)draft.stop();ui.removeCallbacks(tick);renderComposer();awaitTranscript(generation);}
                public void onError(int error){if(generation==speechGeneration)dictationFailed();}
                public void onResults(Bundle results){if(generation==speechGeneration)finishDictation(results.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION));}
                public void onPartialResults(Bundle results){if(generation!=speechGeneration)return;ArrayList<String> matches=results.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION);if(matches!=null&&!matches.isEmpty()){typed=limit(matches.get(0));if(partial!=null)partial.setText(typed);if(textInput!=null&&!textInput.isEnabled())textInput.setText(typed);}}
                public void onEvent(int type,Bundle p){}
            });
            Intent request=new Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH);
            request.putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL,RecognizerIntent.LANGUAGE_MODEL_FREE_FORM);
            request.putExtra(RecognizerIntent.EXTRA_LANGUAGE,Locale.getDefault().toLanguageTag());
            request.putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE,true);request.putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS,true);
            recognizer.startListening(request);renderComposer();ui.post(tick);
        } catch(RuntimeException e){releaseSpeech();draft.state=Draft.State.IDLE;speechIssue="Dictation unavailable.";renderComposer();}
    }
    @Override public void onRequestPermissionsResult(int request,String[] permissions,int[] grants){super.onRequestPermissionsResult(request,permissions,grants);if(request==4&&grants.length>0&&grants[0]==PackageManager.PERMISSION_GRANTED){openRequest=true;ui.post(()->maybeRecord());}else if(request==4)toast("You can type without microphone access.");}
    private final Runnable tick=new Runnable(){public void run(){if(recognizer==null||draft.state!=Draft.State.RECORDING||!foreground)return;duration=elapsed();if(duration>=120){stopRecording();return;}timer.setText("Listening · "+clock(duration));ui.postDelayed(this,1000);}};
    private int elapsed(){return Math.min(120,Math.max(1,(int)((SystemClock.elapsedRealtime()-started)/1000)));}
    private void stopRecording(){if(!draft.stop())return;ui.removeCallbacks(tick);duration=elapsed();if(recognizer!=null){try{recognizer.stopListening();}catch(RuntimeException e){releaseSpeech();}awaitTranscript(speechGeneration);}renderComposer();}
    private void awaitTranscript(int generation){ui.postDelayed(()->{if(generation==speechGeneration&&recognizer!=null&&draft.state==Draft.State.REVIEW){releaseSpeech();speechIssue=typed.trim().isEmpty()?"Didn't catch that.":"Review partial transcript.";renderComposer();}},5000);}
    private void releaseSpeech(){speechGeneration++;if(recognizer!=null){SpeechRecognizer old=recognizer;recognizer=null;old.cancel();old.destroy();}}
    private static String limit(String text){return text.substring(0,Math.min(4096,text.length()));}
    private void dictationFailed(){releaseSpeech();ui.removeCallbacks(tick);if(draft.state==Draft.State.RECORDING)draft.stop();speechIssue=typed.trim().isEmpty()?"Couldn't transcribe.":"Review partial transcript.";renderComposer();}
    private void finishDictation(ArrayList<String> matches){if(draft.state!=Draft.State.RECORDING&&draft.state!=Draft.State.REVIEW)return;if(matches!=null&&!matches.isEmpty())typed=limit(matches.get(0));speechIssue=typed.trim().isEmpty()?"Didn't catch that.":"";if(textInput!=null)textInput.setText(typed);releaseSpeech();ui.removeCallbacks(tick);if(draft.state==Draft.State.RECORDING)draft.stop();renderComposer();}
    private void discard(){if(!draft.cancel())return;releaseSpeech();ui.removeCallbacks(tick);typed="";speechIssue="";if(textInput!=null)textInput.setText("");hideKeyboard();renderComposer();}
    private void keyboard(){
        if(draft.state==Draft.State.SENDING||draft.state==Draft.State.UNCERTAIN)return;
        if(draft.state==Draft.State.RECORDING){if(preview)draft.stop();else stopRecording();}
        releaseSpeech();if(textInput!=null)textInput.setText(typed);draft.state=Draft.State.TEXT;speechIssue="";messageSent=false;renderComposer();
        EditText editor=textInput;editor.requestFocus();
        ui.postDelayed(()->{if(foreground&&draft.state==Draft.State.TEXT&&textInput==editor&&editor.isAttachedToWindow())getSystemService(InputMethodManager.class).showSoftInput(editor,InputMethodManager.SHOW_IMPLICIT);},150);
    }
    private void hideKeyboard(){getSystemService(InputMethodManager.class).hideSoftInputFromWindow(root.getWindowToken(),0);}
    private void sendText(){
        if((draft.state!=Draft.State.REVIEW&&draft.state!=Draft.State.TEXT)||recognizer!=null||textInput==null)return;
        String value=textInput.getText().toString().trim();if(value.isEmpty()){toast("Enter a message before sending.");return;}
        if(preview){toast("Preview — nothing sent");return;}
        openRequest=false;handoff.submitted();
        if(telegram.sendText(value)){draft.send();pendingLast=true;hideKeyboard();renderComposer();}
        else handoff.abandon();
    }
    private void renderLogin(){
        String key=telegram.configured()?telegram.candidate!=null?"confirm":telegram.auth==null?"loading":telegram.auth.getClass().getSimpleName():"configure";
        if(!key.equals(screen)){
            captureVisible=false;screen=key;base();ScrollView view=new ScrollView(this);LinearLayout form=column();form.setPadding(dp(26),dp(40),dp(26),dp(24));view.addView(form);root.addView(view,new LinearLayout.LayoutParams(-1,-1));
            TextView name=text("Betty",32,TEXT,true);name.setPadding(0,0,0,dp(20));form.addView(name);status=text(telegram.status,16,MUTED,false);status.setLineSpacing(dp(4),1);form.addView(status);
            if(key.equals("configure")){
                EditText id=input("Telegram app API ID",false);id.setInputType(InputType.TYPE_CLASS_NUMBER);form.addView(id);
                EditText hash=input("Telegram app API hash",true);form.addView(hash);
                EditText username=input("Betty's bot username",false);username.setFilters(new InputFilter[]{new InputFilter.LengthFilter(32)});form.addView(username);
                form.addView(primary("Connect Telegram",()->{try{telegram.configure(Integer.parseInt(id.getText().toString().trim()),hash.getText().toString().trim(),username.getText().toString().trim().replaceFirst("^@",""));hash.setText("");changed();}catch(Exception e){toast("Check the API ID, API hash and bot username.");}}));
                form.addView(secondary("Get API credentials",()->startActivity(new Intent(Intent.ACTION_VIEW,android.net.Uri.parse("https://my.telegram.org/apps")))));
            }else if(key.equals("confirm")){
                TextView bot=text(telegram.candidate.title+"\n@"+telegram.config.optString("username"),20,TEXT,true);bot.setPadding(0,dp(24),0,dp(16));form.addView(bot);
                form.addView(text("Confirm this is your existing Betty conversation.",14,MUTED,false));form.addView(primary("Use this conversation",()->telegram.confirmChat()));
            }else if(telegram.auth instanceof TdApi.AuthorizationStateWaitPhoneNumber||telegram.auth instanceof TdApi.AuthorizationStateWaitCode||telegram.auth instanceof TdApi.AuthorizationStateWaitPassword||telegram.auth instanceof TdApi.AuthorizationStateWaitEmailAddress||telegram.auth instanceof TdApi.AuthorizationStateWaitEmailCode){
                boolean secret=telegram.auth instanceof TdApi.AuthorizationStateWaitPassword;
                String placeholder=secret?"Two-step verification password":telegram.auth instanceof TdApi.AuthorizationStateWaitPhoneNumber?"Phone number including country code":telegram.auth instanceof TdApi.AuthorizationStateWaitEmailAddress?"Email address":"Telegram login code";
                EditText value=input(placeholder,true);if(secret)value.setFilters(new InputFilter[0]);if(telegram.auth instanceof TdApi.AuthorizationStateWaitPhoneNumber)value.setInputType(InputType.TYPE_CLASS_PHONE);form.addView(value);
                form.addView(primary("Continue",()->{String answer=Login.input(value.getText().toString(),secret);if(!answer.isEmpty()){telegram.authenticate(answer);value.setText("");hideKeyboard();}}));
            }
        }
        status.setText(telegram.status);
    }
    private void fatal(String message){base();TextView t=text(message==null?"Unable to start Betty.":message,16,TEXT,false);t.setPadding(dp(24),dp(48),dp(24),0);root.addView(t);}
    private void showMenu(){new AlertDialog.Builder(this).setTitle("Betty").setItems(new String[]{"Open Telegram","Assistant settings","Sign out"},(d,which)->{if(which==0)openTelegram(false);else if(which==1)assistantSettings();else if(telegram!=null)new AlertDialog.Builder(this).setTitle("Sign out of Betty?").setNegativeButton("Cancel",null).setPositiveButton("Sign out",(dialog,w)->{if(draft.state==Draft.State.RECORDING)discard();telegram.logout();}).show();}).show();}
    private void assistantSettings(){RoleManager roles=getSystemService(RoleManager.class);if(roles.isRoleAvailable(RoleManager.ROLE_ASSISTANT)&&!roles.isRoleHeld(RoleManager.ROLE_ASSISTANT))startActivityForResult(roles.createRequestRoleIntent(RoleManager.ROLE_ASSISTANT),9);else startActivity(new Intent(Settings.ACTION_VOICE_INPUT_SETTINGS));}
    private void openTelegram(boolean afterSend){
        if(preview||handoffBusy)return;
        handoffBusy=true;
        telegram.request(new TdApi.SearchPublicChat(telegram.config.optString("username")),result->{
            handoffBusy=false;
            if(isFinishing())return;
            if(!foreground){if(afterSend)handoff.defer();return;}
            if(!(result instanceof TdApi.Chat)||((TdApi.Chat)result).id!=telegram.chatId){toast("Open your saved Betty conversation in Telegram directly.");return;}
            Intent intent=new Intent(Intent.ACTION_VIEW,android.net.Uri.parse(TelegramLink.chat(telegram.config.optString("username"))));intent.setPackage("org.telegram.messenger");
            try{startActivity(intent);if(afterSend)finish();}catch(android.content.ActivityNotFoundException e){toast("Telegram is unavailable. Your message will not be sent again.");}
        });
    }
    private int dp(float value){return Math.round(value*getResources().getDisplayMetrics().density);}
    private LinearLayout column(){LinearLayout l=new LinearLayout(this);l.setOrientation(LinearLayout.VERTICAL);return l;}
    private LinearLayout row(){LinearLayout l=new LinearLayout(this);l.setOrientation(LinearLayout.HORIZONTAL);l.setGravity(Gravity.CENTER_VERTICAL);return l;}
    private LinearLayout.LayoutParams box(int w,int h){return new LinearLayout.LayoutParams(dp(w),dp(h));}
    private LinearLayout.LayoutParams weightedButton(){LinearLayout.LayoutParams p=new LinearLayout.LayoutParams(0,dp(56),1);p.setMargins(dp(10),0,dp(10),0);return p;}
    private TextView text(String value,int size,int color,boolean bold){TextView t=new TextView(this);t.setText(value);t.setTextSize(size);t.setTextColor(color);if(bold)t.setTypeface(Typeface.create("sans-serif-medium",Typeface.NORMAL));return t;}
    private GradientDrawable round(int color,float radius){GradientDrawable g=new GradientDrawable();g.setColor(color);g.setCornerRadius(dp(radius));return g;}
    private Button primary(String value,Runnable action){Button b=new Button(this);b.setText(value);b.setTextSize(16);b.setTextColor(BG);b.setAllCaps(false);b.setTypeface(Typeface.create("sans-serif-medium",Typeface.NORMAL));b.setBackground(round(MINT,28));b.setPadding(dp(16),0,dp(16),0);b.setMinHeight(dp(56));b.setOnClickListener(v->action.run());return b;}
    private Button secondary(String value,Runnable action){Button b=primary(value,action);b.setBackground(round(PANEL,24));b.setTextColor(MINT);return b;}
    private ImageButton icon(int drawable,String description,Runnable action){ImageButton b=new ImageButton(this);b.setImageResource(drawable);b.setImageTintList(android.content.res.ColorStateList.valueOf(TEXT));b.setBackground(null);b.setContentDescription(description);b.setPadding(dp(12),dp(12),dp(12),dp(12));b.setOnClickListener(v->action.run());return b;}
    private EditText input(String placeholder,boolean secret){EditText t=new EditText(this);t.setHint(placeholder);t.setTextColor(TEXT);t.setHintTextColor(MUTED);t.setTextSize(16);t.setSingleLine(true);t.setInputType(InputType.TYPE_CLASS_TEXT|(secret?InputType.TYPE_TEXT_VARIATION_PASSWORD:InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS));t.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);t.setSaveEnabled(false);t.setFilters(new InputFilter[]{new InputFilter.LengthFilter(256)});t.setPadding(dp(10),dp(14),dp(10),dp(14));return t;}
    private void toast(String message){Toast.makeText(this,message,Toast.LENGTH_LONG).show();}
    private static String clock(int seconds){return String.format(Locale.ROOT,"%d:%02d",seconds/60,seconds%60);}
}
