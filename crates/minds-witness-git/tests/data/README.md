# Testdaten für `minds-witness-git`

- `golden_witness.pub`: öffentlicher Ed25519-Testschlüssel (nur für Tests; der
  private Schlüssel wurde nach dem Erzeugen der Signatur verworfen und liegt
  nirgends im Repository).
- `golden_binding`: kanonische Bytes der Golden-Bindung aus s2_01.
- `golden_binding.sig`: `ssh-keygen -Y sign -n minds-witness` über
  `golden_binding` mit diesem Schlüssel (s2_03).
