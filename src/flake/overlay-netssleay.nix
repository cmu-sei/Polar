final: prev: {
  perl540Packages = prev.perl540Packages // {
    NetSSLeay = prev.perl540Packages.NetSSLeay.overrideAttrs (old: {
      # Pre-configure steps to help Net::SSLeay find the replaced openssl outputs
      preConfigure = ''
        ${old.preConfigure or ""}

        mkdir openssl
        # Use final.openssl.* so that we refer to the final replaced openssl package
        ln -s ${final.openssl.out}/lib openssl/lib
        ln -s ${final.openssl.bin}/bin openssl/bin
        ln -s ${final.openssl.dev}/include openssl/include

        export OPENSSL_PREFIX=$(realpath openssl)
        export OPENSSL_INCLUDE="$OPENSSL_PREFIX/include"
        export OPENSSL_LIB="$OPENSSL_PREFIX/lib"
      '';

      # If devdoc or multiple outputs cause trouble, disable them:
      dontSeparateOutputs = true;
    });
  };
}
