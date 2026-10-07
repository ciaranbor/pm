# Firebase (the google flavour's push distributor) instantiates the registrars named in the
# manifest by reflection. Its own rule keeps the classes but, in R8 full mode, not the constructors.
-keep class * implements com.google.firebase.components.ComponentRegistrar { <init>(); }
