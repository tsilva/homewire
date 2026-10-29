package eu.tsilva.homewire.betty.tests;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.ComponentName;
import android.content.Intent;
import android.os.Bundle;
import android.os.SystemClock;
import android.view.View;
import android.view.ViewGroup;
import android.widget.EditText;
import android.widget.TextView;
import java.io.File;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import org.json.JSONObject;

/** Synthetic, offline checks. Run only on a disposable emulator with no account. */
public final class Checks extends Instrumentation {
    private int passed;
    private Activity activity;
    @Override public void onCreate(Bundle args){super.onCreate(args);start();}
    private void check(boolean condition,String label){if(!condition)throw new AssertionError(label);passed++;}
    private Object field(Object o,String name)throws Exception{Field f=o.getClass().getDeclaredField(name);f.setAccessible(true);return f.get(o);}
    private void set(Object o,String name,Object v)throws Exception{Field f=o.getClass().getDeclaredField(name);f.setAccessible(true);f.set(o,v);}
    private void call(Object o,String name)throws Exception{Method m=o.getClass().getDeclaredMethod(name);m.setAccessible(true);m.invoke(o);}
    private void state(String value)throws Exception{Object d=field(activity,"draft");set(d,"state",java.lang.Enum.valueOf((Class)field(d,"state").getClass(),value));}
    private String state()throws Exception{return field(field(activity,"draft"),"state").toString();}
    private void main(Runnable r){runOnMainSync(r);waitForIdleSync();}
    private String labels(View v){String s=v instanceof TextView?((TextView)v).getText().toString()+"\n":"";if(v instanceof ViewGroup){ViewGroup g=(ViewGroup)v;for(int i=0;i<g.getChildCount();i++)s+=labels(g.getChildAt(i));}return s;}
    private View control(View view,String label){
        if(view instanceof TextView&&((TextView)view).getText().toString().equals(label))return view;
        if(label.equals(view.getContentDescription()))return view;
        if(view instanceof ViewGroup){ViewGroup group=(ViewGroup)view;for(int i=0;i<group.getChildCount();i++){View found=control(group.getChildAt(i),label);if(found!=null)return found;}}
        return null;
    }
    @Override public void onStart(){
        Bundle results=new Bundle();
        try{
            Intent launch=new Intent().setComponent(new ComponentName("eu.tsilva.homewire.betty","eu.tsilva.homewire.betty.MainActivity"));launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            activity=startActivitySync(launch);waitForIdleSync();
            Object telegram=field(activity,"telegram");
            check(telegram!=null,"Pinned native TDLib libraries loaded");
            check(field(telegram,"client")==null,"No client before credentials are configured");
            check(field(activity,"recognizer")==null,"No microphone capture before setup");
            main(()->{try{set(activity,"openRequest",false);set(telegram,"ready",true);set(telegram,"connection","Connected");call(activity,"changed");}catch(Exception e){throw new RuntimeException(e);}});
            String ui=labels((View)field(activity,"root"));
            check(ui.contains("Betty")&&ui.contains("Record"),"Capture-only UI is available");
            check(!ui.contains("Today")&&!ui.contains("Continue the conversation"),"Chat history is absent");
            main(()->{try{state("RECORDING");set(activity,"started",SystemClock.elapsedRealtime());set(activity,"typed","Synthetic partial transcript");call(activity,"renderComposer");}catch(Exception e){throw new RuntimeException(e);}});
            check(labels((View)field(activity,"root")).contains("Synthetic partial transcript"),"Live transcription has a visible surface");
            check(!labels((View)field(activity,"root")).contains("\nSend\n"),"Recording has no send action");
            main(()->{try{Method m=activity.getClass().getDeclaredMethod("finishDictation",java.util.ArrayList.class);m.setAccessible(true);m.invoke(activity,new java.util.ArrayList<String>(java.util.Arrays.asList("Synthetic final transcript")));}catch(Exception e){throw new RuntimeException(e);}});
            check(state().equals("REVIEW"),"Final transcription enters review");
            EditText input=(EditText)field(activity,"textInput");
            check(input.getText().toString().equals("Synthetic final transcript"),"Transcript is shown before send");
            check(input.isEnabled(),"Final transcript is editable");
            check(control((View)field(activity,"root"),"Send").isEnabled(),"Nonempty final transcript enables Send");
            check(!labels((View)field(activity,"root")).contains("Review your message"),"Review has no repeated heading");
            check(!(Boolean)field(telegram,"sendBusy"),"Recognition never sends automatically");
            main(()->{try{input.setText("Edited synthetic transcript");Method m=activity.getClass().getDeclaredMethod("onNewIntent",Intent.class);m.setAccessible(true);m.invoke(activity,new Intent(Intent.ACTION_ASSIST));}catch(Exception e){throw new RuntimeException(e);}});
            check(state().equals("REVIEW")&&input.getText().toString().equals("Edited synthetic transcript"),"Opening again preserves the unsent edit");
            main(()->input.setText("   "));
            check(!control((View)field(activity,"root"),"Send").isEnabled(),"Whitespace cannot be sent");
            main(()->input.setText("Corrected synthetic transcript"));
            check(control((View)field(activity,"root"),"Send").isEnabled(),"Editing re-enables Send");
            main(()->{try{call(activity,"discard");state("RECORDING");set(activity,"typed","");call(activity,"renderComposer");call(activity,"dictationFailed");}catch(Exception e){throw new RuntimeException(e);}});
            check(state().equals("REVIEW")&&!control((View)field(activity,"root"),"Send").isEnabled(),"Failed empty transcription keeps Send disabled");
            check(control((View)field(activity,"root"),"Record again")!=null,"Empty review has a direct recording retry");
            check(labels((View)field(activity,"root")).contains("Couldn't transcribe."),"Transcription failure uses a short inline message");
            main(()->{try{call(activity,"discard");call(activity,"keyboard");}catch(Exception e){throw new RuntimeException(e);}});
            check(state().equals("TEXT"),"Keyboard composition remains available");
            main(()->{try{state("RECORDING");set(activity,"started",SystemClock.elapsedRealtime());call(activity,"renderComposer");}catch(Exception e){throw new RuntimeException(e);}});
            getTargetContext().startActivity(new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));Thread.sleep(300);waitForIdleSync();
            check(state().equals("REVIEW"),"Leaving foreground stops capture into review");
            check(field(activity,"recognizer")==null,"No recognizer remains in background");
            // An actual success callback, delivered while backgrounded, must defer navigation.
            main(()->{try{Object h=field(activity,"handoff");call(h,"submitted");set(telegram,"sendBusy",true);set(activity,"pendingLast",true);state("SENDING");call(telegram,"sent");}catch(Exception e){throw new RuntimeException(e);}});
            check((Boolean)field(activity,"messageSent"),"Confirmed TDLib delivery reaches the capture UI");
            check(state().equals("IDLE"),"Confirmed send clears the reviewed draft");
            check((Boolean)field(field(activity,"handoff"),"ready"),"Background delivery defers Telegram navigation");
            check(!(Boolean)field(activity,"handoffBusy"),"Background delivery starts no navigation request");
            main(()->{try{call(field(activity,"handoff"),"abandon");set(telegram,"sendBusy",false);set(telegram,"sendUnknown",true);call(activity,"changed");}catch(Exception e){throw new RuntimeException(e);}});
            check(state().equals("UNCERTAIN"),"Unconfirmed delivery shows a warning");
            check(!(Boolean)field(field(activity,"handoff"),"ready"),"Uncertain delivery cannot open Telegram automatically");
            // Force a bounded-storage failure with synthetic oversized state, not real credentials.
            main(()->{try{
                JSONObject config=(JSONObject)field(telegram,"config");
                config.put("synthetic_overflow",new String(new char[9000]).replace('\0','x'));
                call(field(activity,"handoff"),"submitted");set(activity,"messageSent",false);
                set(telegram,"sendUnknown",false);set(telegram,"sendBusy",true);call(telegram,"sent");
            }catch(Exception e){throw new RuntimeException(e);}});
            check((Boolean)field(telegram,"sendUnknown")&&state().equals("UNCERTAIN"),"Storage failure retains the send warning");
            check(!(Boolean)field(field(activity,"handoff"),"ready"),"Storage failure cannot trigger navigation");
            check(((JSONObject)field(telegram,"config")).optBoolean("pending"),"Storage failure preserves pending protection");
            ((JSONObject)field(telegram,"config")).remove("synthetic_overflow");
            // Keystore test state is synthetic and removed before completion.
            Object vault=field(telegram,"vault");Method write=vault.getClass().getDeclaredMethod("write",JSONObject.class);write.setAccessible(true);Method read=vault.getClass().getDeclaredMethod("read");read.setAccessible(true);
            write.invoke(vault,new JSONObject().put("test_marker","synthetic-encryption-check"));
            check(((JSONObject)read.invoke(vault)).getString("test_marker").equals("synthetic-encryption-check"),"Keystore round trip succeeds");
            File sealed=new File(getTargetContext().getNoBackupFilesDir(),"session.sealed");byte[] bytes=java.nio.file.Files.readAllBytes(sealed.toPath());
            check(!new String(bytes,java.nio.charset.StandardCharsets.ISO_8859_1).contains("synthetic-encryption-check"),"Protected storage has no plaintext marker");
            check(sealed.delete(),"Synthetic state is removed");
            results.putString("checks",Integer.toString(passed));results.putString("result","passed");finish(Activity.RESULT_OK,results);
        }catch(Throwable failure){results.putString("result","failed");results.putString("failure",failure.toString());finish(Activity.RESULT_CANCELED,results);}
    }
}
