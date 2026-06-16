import 'package:meta/meta.dart';
export 'serializable.dart';

/// A domain model representing a contact in the system.
class Contact {
  final String id;
  final String name;
  final String email;

  Contact(this.id, this.name, this.email);

  Contact.fromJson(Map<String, dynamic> json)
      : id = json['id'] as String,
        name = json['name'] as String,
        email = json['email'] as String;

  factory Contact.empty() => Contact('', '', '');

  Map<String, dynamic> toJson() => {
        'id': id,
        'name': name,
        'email': email,
      };
}
