# ML Kit and Firebase instantiate the registrars named in the manifest by reflection. Their own
# rule keeps the classes but, in R8 full mode, not the constructors.
-keep class * implements com.google.firebase.components.ComponentRegistrar { <init>(); }
