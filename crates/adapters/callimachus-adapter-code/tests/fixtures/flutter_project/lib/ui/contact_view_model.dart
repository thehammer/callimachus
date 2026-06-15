import 'package:flutter_riverpod/flutter_riverpod.dart';
import '../domain/contact.dart';
import '../domain/serializable.dart';

/// Disposable resources can be cleaned up.
abstract class Disposable {
  void dispose();
}

/// ViewModel for the contact list screen.
///
/// Extends StateNotifier to manage reactive state, implements Disposable
/// for resource cleanup, and uses the Timestamped mixin for audit fields.
class ContactViewModel extends StateNotifier<List<Contact>>
    with Timestamped
    implements Disposable {
  @override
  final DateTime createdAt;
  @override
  final DateTime updatedAt;

  ContactViewModel()
      : createdAt = DateTime.now(),
        updatedAt = DateTime.now(),
        super([]);

  Future<void> load() async {
    final contacts = await _fetchContacts();
    state = contacts;
  }

  @override
  void dispose() {
    super.dispose();
  }

  Future<List<Contact>> _fetchContacts() async {
    return [];
  }
}
